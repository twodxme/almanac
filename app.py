#!/usr/bin/env python3
"""almanac — one window for everything installed on this machine.

Design rule: this app owns the VIEW and the AppImage registry. It does not own
package state: installs and updates are handed to the tool that owns them
(yay, pacman, mise, git), with root requested through polkit, and a full
upgrade runs in a real terminal through the system's own updater. Every
result is then checked rather than assumed -- see verify.py.
"""
from __future__ import annotations

import sys

# `almanac --guard` is the background settings watcher: no window, so it is
# dispatched before GTK is even imported.
if __name__ == "__main__" and "--guard" in sys.argv[1:]:
    import watcher
    sys.exit(watcher.main())

import gi  # noqa: E402

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("GdkPixbuf", "2.0")
gi.require_version("Pango", "1.0")
from gi.repository import (Adw, Gdk, GdkPixbuf, Gio, GLib, GObject, Gtk,  # noqa: E402
                           Pango)

import shutil  # noqa: E402
import subprocess  # noqa: E402
import json  # noqa: E402
import os  # noqa: E402
import threading  # noqa: E402
import time  # noqa: E402
from concurrent.futures import ThreadPoolExecutor  # noqa: E402
from datetime import datetime  # noqa: E402
from pathlib import Path  # noqa: E402

import appimage as appimage_mod  # noqa: E402
import inventory
import store as store_mod  # noqa: E402
import tasks  # noqa: E402
import maintenance  # noqa: E402
import selfupdate  # noqa: E402
import theming  # noqa: E402
import featured  # noqa: E402
import ai  # noqa: E402
import agents as agents_mod  # noqa: E402
import autoupdate  # noqa: E402
import ghcache  # noqa: E402
import icons as icons_mod  # noqa: E402
import guard  # noqa: E402
import history  # noqa: E402
import mimeguard  # noqa: E402
import patches  # noqa: E402
import snapshots  # noqa: E402
import sources  # noqa: E402
import verify  # noqa: E402
import watcher  # noqa: E402
import xdg  # noqa: E402
import shlex  # noqa: E402

# Install is an arrow into a tray, shipped in icons/: the theme's own download
# icon is a folder, which reads as "the Downloads folder".
INSTALL_ICON = "almanac-install-symbolic"

_CSS = b"""
/* Sidebar counts. Three levels: a count is usually just a number, an
   amber one wants looking at, and only a security update earns red. The
   label is vertically centred at the widget level -- without that the
   background fills the whole row and the pill reads as a tall oval. */
.count-badge {
  background: alpha(@window_fg_color, 0.10);
  color: @window_fg_color;
  border-radius: 999px;
  font-size: 0.72rem;
  font-weight: bold;
  font-feature-settings: "tnum";
  padding: 1px 7px;
  min-width: 16px;
  min-height: 0;
}
.count-badge.is-warn {
  background: alpha(@warning_color, 0.22);
  color: @warning_color;
}
.count-badge.is-crit {
  /* The only solid fill in the sidebar, so it is the only thing that
     shouts. Its foreground is measured against the fill, not assumed. */
  background: @error_color;
  color: @error_fg_color;
}
row:selected .count-badge {
  background: alpha(@accent_fg_color, 0.18);
  color: @accent_fg_color;
}
row:selected .count-badge.is-crit {
  background: @error_color;
  color: @error_fg_color;
}
/* ---- pills ----
   One shape, one size, five meanings. There were nine classes across three
   font sizes doing this job -- crit-pill, stale-pill, crit-core, store-chip,
   store-installed and three trust-* variants -- which is why a status chip
   and a provenance chip on the same card never quite lined up.

   Each modifier carries the base declarations too, so a single class name is
   enough at every call site and nothing has to remember to add two. */
.pill, .pill-crit, .pill-warn, .pill-ok, .pill-accent, .pill-quiet {
  background: alpha(@window_fg_color, 0.09);
  border-radius: 999px;
  padding: 1px 8px;
  font-size: 0.70rem;
}
.pill-crit {
  background: alpha(@error_color, 0.18);
  color: @error_color;
  font-weight: bold;
}
.pill-warn {
  background: alpha(@warning_color, 0.20);
  color: @warning_color;
}
.pill-ok {
  background: alpha(@success_color, 0.18);
  color: @success_color;
  font-weight: bold;
}
.pill-accent {
  background: alpha(@accent_color, 0.20);
  color: @accent_color;
  font-weight: bold;
}
.pill-quiet {
  opacity: 0.7;
}
.link-ish {
  color: @accent_color;
}

/* ---- plugin marketplace cards ---- */
.store-card {
  background: @card_bg_color;
  border: 1px solid alpha(@window_fg_color, 0.13);
  border-radius: 12px;
}
.store-card:hover {
  border-color: alpha(@accent_color, 0.75);
  background: alpha(@accent_color, 0.04);
}
.store-shot {
  border-radius: 11px 11px 0 0;
  background: alpha(@window_fg_color, 0.05);
}
/* The catalog assigns one of six accents per plugin; the ~800 entries with no
   screenshot get an initials tile in theirs, so every card still reads as a
   card. Six static classes rather than a CSS provider per card. */
.acc-cyan   { background: linear-gradient(135deg, #2dd4bf, #0e7490); }
.acc-coral  { background: linear-gradient(135deg, #fb7185, #9f1239); }
.acc-lime   { background: linear-gradient(135deg, #a3e635, #3f6212); }
.acc-rose   { background: linear-gradient(135deg, #f472b6, #9d174d); }
.acc-amber  { background: linear-gradient(135deg, #fbbf24, #b45309); }
.acc-violet { background: linear-gradient(135deg, #a78bfa, #5b21b6); }
.acc-none   { background: linear-gradient(135deg, #94a3b8, #334155); }
.store-initials {
  font-size: 1.7rem;
  font-weight: bold;
  color: #ffffff;
}
.store-name { font-weight: bold; }
.store-verified { color: @success_color; }
.store-stars { font-size: 0.72rem; }
.stats { font-size: 0.74rem; }
.stat-key { opacity: 0.62; }
.stat-val {
  font-weight: bold;
  font-feature-settings: "tnum";
}
.hero { border-radius: 12px; }
/* The scrim is what keeps the text legible over an arbitrary wallpaper --
   dark at the bottom where the type sits, clear at the top. */
.hero-scrim {
  background: linear-gradient(to top,
              alpha(#000, 0.78) 0%,
              alpha(#000, 0.42) 45%,
              alpha(#000, 0.10) 100%);
}
.hero-head {
  font-size: 1.45rem;
  font-weight: bold;
  color: #ffffff;
}
.hero-sub {
  font-size: 0.86rem;
  color: alpha(#ffffff, 0.82);
}
.store-prov {
  font-size: 0.68rem;
  opacity: 0.75;
}
/* Provenance chips. Four states rather than a tick, because "reviewed",
   "verified", "reviewed but upstream has moved since" and "nobody checked"
   are four different promises. */
"""

SOURCE_LABEL = {
    inventory.REPO: "Repo",
    inventory.AUR: "AUR",
    inventory.APPIMAGE: "AppImage",
    inventory.LOCAL: "Local",
    inventory.FLATPAK: "Flatpak",
    inventory.ORPHAN: "Orphan",
    inventory.SYSTEM: "System",
    inventory.AVAILABLE: "Install",
    inventory.PLUGIN: "Plugin",
    inventory.AGENT: "AI agent",
    inventory.TOOL: "Dev tool",
}
_FALLBACK_ICON = {
    inventory.REPO: "package-x-generic",
    inventory.AUR: "package-x-generic",
    inventory.APPIMAGE: "application-x-executable",
    inventory.LOCAL: "applications-engineering",
    inventory.FLATPAK: "application-x-executable",
    inventory.ORPHAN: "user-trash-symbolic",
    inventory.SYSTEM: "preferences-system-symbolic",
    inventory.AVAILABLE: "system-software-install-symbolic",
    inventory.PLUGIN: "application-x-addon-symbolic",
    inventory.AGENT: "system-run-symbolic",
    inventory.TOOL: "applications-engineering-symbolic",
}


def _age(ts: float) -> str:
    """Epoch -> "3d ago". An age answers "is this stale?"; a timestamp doesn't."""
    if not ts:
        return ""
    days = (datetime.now() - datetime.fromtimestamp(ts)).days
    if days < 1:
        return "today"
    if days < 30:
        return f"{days}d ago"
    if days < 365:
        return f"{days // 30}mo ago"
    return f"{days / 365:.1f}y ago"


def _primary_category(cats: str) -> str:
    """One meaningful word from a Categories= list.

    Generic buckets lead most .desktop files, so taking the first entry would
    label nearly everything "Utility". Prefer whichever specific one appears.
    """
    if not cats:
        return ""
    parts = [c for c in cats.split(";") if c]
    generic = {"Application", "Utility", "System", "Settings", "GTK", "Qt",
               "GNOME", "KDE", "ConsoleOnly"}
    for c in parts:
        if c not in generic:
            return c
    return parts[0] if parts else ""


def _fmt_size(mb: float) -> str:
    if not mb:
        return ""
    if mb >= 1024:
        return f"{mb / 1024:.1f} GB"
    if mb >= 1:
        return f"{mb:.0f} MB"
    return f"{mb * 1024:.0f} KB"


class _reverse_text(str):
    """Sorts as the reverse of the string it wraps.

    Gtk.CustomSorter has no descending flag, and negating a string is not a
    thing, so reversing the comparison is the way to get Z -> A.
    """
    def __lt__(self, other): return str.__gt__(self, other)
    def __gt__(self, other): return str.__lt__(self, other)


def _version_text(it) -> str:
    if getattr(it, "git_behind", 0):
        suffix = " · local edits" if it.git_dirty else ""
        return f"{it.app.version} · {it.git_behind} commits behind{suffix}"
    if not it.has_update:
        return it.app.version
    behind = getattr(it, "behind", 0)
    kind = getattr(it, "behind_kind", "")
    short = {"major": "maj", "minor": "min", "patch": "pt", "point": "pt"}
    suffix = ""
    if behind > 0 and kind in short:
        suffix = f"  ·{behind} {short[kind]}"
    elif kind == "rel":
        suffix = "  ·rebuild"
    return f"{it.app.version} → {it.app.update}{suffix}"


class AppItem(GObject.Object):
    """GObject wrapper so inventory.App can live in a Gio.ListStore."""
    __gtype_name__ = "AppItem"

    def __init__(self, app: inventory.App):
        super().__init__()
        self.app = app
        e = app.extra
        # Flattened for the sorters, which must be cheap: they run on every
        # comparison during a sort.
        self.name = app.name
        self.category = _primary_category(e.get("categories", "")) or (
            "Local build" if app.source == inventory.LOCAL else "")
        self.size_mb = float(e.get("size_mb", 0.0))
        self.installed_ts = float(e.get("installed_ts", 0.0))
        self.source = app.source
        self.has_update = app.has_update
        self.critical = e.get("critical", "")
        self.stale = e.get("stale", "")
        self.advice = e.get("advice", "")
        self.behind, self.behind_kind = e.get("behind", (0, ""))
        self.git_behind = int(e.get("git_behind", 0) or 0)
        self.git_dirty = bool(e.get("git_dirty"))
        if self.git_behind:
            # Sorting treats commits-behind as its own magnitude; mixing it
            # with semver counts would compare unlike things.
            self.behind = self.git_behind
        # Critical first, then any update, then the rest -- so the default
        # sort already puts the rows that matter at the top.
        self.priority = 0 if (self.critical or self.stale) else (
            1 if (self.has_update and app.source == inventory.SYSTEM) else
            2 if self.has_update else 3)
        self.votes = int(e.get("votes", 0) or 0)
        self.popularity = float(e.get("popularity", 0) or 0)
        self.added = e.get("added", "") or ""
        self.verified = bool(e.get("verified"))
        self.haystack = f"{app.name} {app.description} {self.category}".lower()


class PluginItem(GObject.Object):
    """A marketplace entry, boxed so a Gio.ListStore can hold it."""

    def __init__(self, plugin):
        super().__init__()
        self.plugin = plugin


class AlmanacWindow(Adw.ApplicationWindow):
    # Grouped by the question each section answers: what can I get, what
    # wants my attention, what do I have, and where did it come from. Eleven
    # peers in one list made all of them equally weighted, which none are.
    GROUPS = (
        # The two primary destinations, ungrouped: getting software and
        # keeping it current are what the app is for, and a header above them
        # would rank them as peers of "Sources".
        ("primary", None, (
            ("featured", "Featured", "starred-symbolic"),
            ("discover", "App Store", "software-store-symbolic"),
            ("themes", "Themes", "preferences-desktop-wallpaper-symbolic"),
            ("updates", "Updates", "software-update-available-symbolic"),
            (inventory.AGENT, "AI", "applications-science-symbolic"),
        )),
        ("library", "Library", (
            ("apps", "Applications", "view-grid-symbolic"),
            ("all", "Everything", "view-list-symbolic"),
        )),
        ("sources", "Sources", (
            (inventory.SYSTEM, "System", "preferences-system-symbolic"),
            (inventory.REPO, "Repositories", "package-x-generic-symbolic"),
            (inventory.AUR, "AUR", "package-x-generic-symbolic"),
            (inventory.APPIMAGE, "AppImages", "application-x-executable-symbolic"),
            (inventory.FLATPAK, "Flatpak", "application-x-executable-symbolic"),
            (inventory.LOCAL, "Local builds", "applications-engineering-symbolic"),
            (inventory.TOOL, "Dev tools", "utilities-terminal-symbolic"),
        )),
        ("maintenance", "Maintenance", (
            ("maintenance", "Health check", "emblem-ok-symbolic"),
            ("history", "History", "document-open-recent-symbolic"),
            (inventory.ORPHAN, "Orphans", "user-trash-symbolic"),
        )),
    )
    # Sources collapse by default: they answer "where did this come from",
    # which is a question you ask occasionally, not on every launch.
    COLLAPSED_BY_DEFAULT = ("sources",)

    # Every page, described once. Before this the header's buttons were shown
    # and hidden from seventeen places across six methods, which is how a page
    # ends up with another page's toolbar.
    #
    #   view     which stack child to show
    #   search   placeholder, or None when the box does nothing here
    #   update   show the "Update all" button
    #   sort     show the sort menu
    PAGES = {
        "featured":    dict(view="featured", title="Featured",
                            search=None, update=False, sort=False,
                            filters=False),
        "discover":    dict(view="store", title="App Store",
                            search="Search plugins and apps…",
                            update=False, sort=False, filters=True),
        # The same view, pinned to one marketplace. Without this themes were
        # reachable only through the filter drawer, and the first one did not
        # appear until the thirtieth card of the default ordering.
        "themes":      dict(view="store", title="Themes",
                            search="Search Omarchy themes…",
                            update=False, sort=False, filters=True,
                            market="themes"),
        "packages":    dict(view="list", title="Software",
                            search="Search installed software",
                            update=True, sort=True, filters=False),
        "maintenance": dict(view="maintenance", title="Health check",
                            search=None, update=False, sort=False, filters=False),
        "history":     dict(view="history", title="History",
                            search=None, update=False, sort=False, filters=False),
        inventory.AGENT: dict(view="ai", title="AI",
                              search=None, update=False, sort=False, filters=False),
    }

    @property
    def CATEGORIES(self):
        return tuple(c for _k, _l, items in self.GROUPS for c in items)

    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self.set_title("Almanac")
        self.set_icon_name("io.github.twodxme.Almanac")
        self.set_default_size(1320, 800)
        # Nothing here should ever need a horizontal scrollbar, so the window
        # has to be able to shrink to a half-screen tile.
        self.set_size_request(480, 420)

        provider = Gtk.CssProvider()
        provider.load_from_data(_CSS)
        Gtk.StyleContext.add_provider_for_display(
            self.get_display(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

        # The Omarchy palette is applied over our own stylesheet, because it
        # redefines the named colours our stylesheet is written against.
        self._theme_provider = Gtk.CssProvider()
        Gtk.StyleContext.add_provider_for_display(
            self.get_display(), self._theme_provider,
            Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION + 1)
        self._apply_theme()
        self._theme_monitors = theming.watch(self._on_theme_changed)

        self.filter_key = "all"
        self.query = ""

        self.store = Gio.ListStore(item_type=AppItem)

        # Shared image infrastructure. Declared here because every page
        # builder below may decode an icon or queue a fetch, and a page
        # builder that runs before its cache exists fails at construction.
        self._tex_cache = {}
        self._thumb_pool = ThreadPoolExecutor(max_workers=6,
                                              thread_name_prefix="thumb")

        # Filter -> sort -> selection. Filtering first means the sorter only
        # ever walks the rows actually on screen.
        self.filter = Gtk.CustomFilter.new(self._match, None)
        filtered = Gtk.FilterListModel(model=self.store, filter=self.filter)
        self.sorted = Gtk.SortListModel(model=filtered)
        selection = Gtk.SingleSelection(model=self.sorted)
        selection.set_can_unselect(True)
        selection.set_autoselect(False)

        # --- sidebar: categories with live counts -------------------------
        # Ten filter chips and five header buttons was most of the window's
        # chrome. A sidebar holds the same navigation with the counts visible
        # at rest, which is the question being asked ("what needs me?").
        self.sidebar_rows = {}
        sidebar_list = Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
        sidebar_list.add_css_class("navigation-sidebar")
        sidebar_list.connect("row-selected", self._on_sidebar_selected)

        self.group_rows = {}
        self.group_members = {}
        for gkey, glabel, items in self.GROUPS:
            if glabel is None:
                self.group_rows[gkey] = None
                self.group_members[gkey] = []
                for key, label, icon in items:
                    row = Adw.ActionRow(title=label)
                    row.set_title_lines(1)
                    row.add_prefix(Gtk.Image(icon_name=icon))
                    count = Gtk.Label(valign=Gtk.Align.CENTER, visible=False)
                    count.add_css_class("count-badge")
                    row.add_suffix(count)
                    row._key, row._count, row._group = key, count, gkey
                    self.sidebar_rows[key] = row
                    self.group_members[gkey].append(row)
                    sidebar_list.append(row)
                continue

            header = Gtk.ListBoxRow(selectable=False, activatable=True)
            hb = Gtk.Box(spacing=6, margin_start=6, margin_top=8,
                         margin_bottom=2, margin_end=6)
            chevron = Gtk.Image(icon_name="pan-down-symbolic")
            chevron.add_css_class("dim-label")
            title = Gtk.Label(label=glabel.upper(), xalign=0, hexpand=True)
            title.add_css_class("caption")
            title.add_css_class("dim-label")
            hb.append(title)
            hb.append(chevron)
            header.set_child(hb)
            header._group, header._chevron = gkey, chevron
            # A single-item group has nothing to fold away; its header is a
            # label, not a control.
            header.set_activatable(len(items) > 1)
            sidebar_list.append(header)
            self.group_rows[gkey] = header
            self.group_members[gkey] = []

            for key, label, icon in items:
                row = Adw.ActionRow(title=label)
                row.set_title_lines(1)
                row.add_prefix(Gtk.Image(icon_name=icon))
                count = Gtk.Label(valign=Gtk.Align.CENTER, visible=False)
                count.add_css_class("count-badge")
                row.add_suffix(count)
                row._key, row._count, row._group = key, count, gkey
                self.sidebar_rows[key] = row
                self.group_members[gkey].append(row)
                sidebar_list.append(row)

        sidebar_list.connect("row-activated", self._on_sidebar_activated)
        self._collapsed = set(self.COLLAPSED_BY_DEFAULT)

        sidebar_scroll = Gtk.ScrolledWindow(vexpand=True)
        sidebar_scroll.set_child(sidebar_list)
        sidebar_scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)

        # A stats footer. The sidebar's bottom half is empty on any normal
        # window height, and these are the numbers you would otherwise open a
        # terminal for.
        self.stats = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2,
                             margin_start=12, margin_end=12,
                             margin_top=8, margin_bottom=12)
        self.stats.add_css_class("stats")
        sidebar_scroll.set_vexpand(True)

        sidebar_page = Adw.ToolbarView()
        sidebar_header = Adw.HeaderBar()
        # The app's own mark, where someone actually looks at it. It was
        # registered with the desktop file and the window manager and so
        # appeared only in a launcher and an alt-tab switcher.
        brand = Gtk.Box(spacing=8, halign=Gtk.Align.CENTER)
        mark = Gtk.Image(icon_name="io.github.twodxme.Almanac", pixel_size=18)
        wordmark = Gtk.Label(label="Almanac")
        wordmark.add_css_class("heading")
        brand.append(mark)
        brand.append(wordmark)
        # Clicking an app's own mark opens its About, as it does in most
        # desktop apps; the menu entry stays for keyboard users.
        brand_btn = Gtk.Button(child=brand, tooltip_text="About Almanac")
        brand_btn.add_css_class("flat")
        brand_btn.update_property([Gtk.AccessibleProperty.LABEL],
                                  ["About Almanac"])
        brand_btn.connect("clicked", self.on_about)
        sidebar_header.set_title_widget(brand_btn)
        sidebar_header.set_show_end_title_buttons(False)
        sidebar_page.add_top_bar(sidebar_header)
        sidebar_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        sidebar_box.append(sidebar_scroll)
        sidebar_box.append(Gtk.Separator())
        sidebar_box.append(self.stats)
        sidebar_page.set_content(sidebar_box)

        # --- content header: one primary action, everything else in a menu --
        toolbar = Adw.ToolbarView()
        header = Adw.HeaderBar()

        self.sidebar_toggle = Gtk.ToggleButton(icon_name="sidebar-show-symbolic")
        self.sidebar_toggle.set_active(True)
        self.sidebar_toggle.set_tooltip_text("Show categories")
        self.sidebar_toggle.connect(
            "toggled", lambda b: self.split.set_show_sidebar(b.get_active()))
        header.pack_start(self.sidebar_toggle)

        # The header names the page and summarises it. Previously the search
        # entry was the title widget, so no page had a name and the four
        # status labels were the only thing describing what you were looking
        # at -- each in a different place on the page.
        self.title = Adw.WindowTitle(title="Almanac", subtitle="")
        title_box = Gtk.Box(spacing=8, halign=Gtk.Align.CENTER)
        self.busy = Gtk.Spinner(valign=Gtk.Align.CENTER)
        self.busy.set_visible(False)
        title_box.append(self.busy)
        title_box.append(self.title)
        header.set_title_widget(title_box)

        self.search = Gtk.SearchEntry(placeholder_text="Search installed software")
        self.search.set_hexpand(True)
        self.search.connect("search-changed", self.on_search)

        # One primary action, always in the same place. Each page says what
        # it is; before this, "Update all" lived in the header while the AI
        # and Maintenance pages had their own button in their own toolbar, in
        # a different position with different alignment.
        self.update_btn = Gtk.Button(label="Update all")
        self.update_btn.add_css_class("suggested-action")
        self.update_btn.connect("clicked", self._on_primary)
        header.pack_end(self.update_btn)
        self._primary = None

        menu = Gio.Menu()
        actions = Gio.Menu()
        actions.append("Install what’s in the search box…", "win.install")
        actions.append("Install from a link…", "win.fromlink")
        actions.append("Track an AppImage file…", "win.track")
        menu.append_section(None, actions)
        maintenance = Gio.Menu()
        maintenance.append("Review orphaned packages…", "win.clean")
        maintenance.append("Refresh this page", "win.rescan")
        menu.append_section(None, maintenance)
        settings = Gio.Menu()
        settings.append("Automatic updates…", "win.prefs")
        menu.append_section(None, settings)
        about_section = Gio.Menu()
        about_section.append("Update Almanac…", "win.selfupdate")
        about_section.append("About Almanac", "win.about")
        menu.append_section(None, about_section)

        self.SORTS_INSTALLED = (
            ("default", "Priority (default)"), ("name", "Name A → Z"),
            ("name_desc", "Name Z → A"), ("size", "Size"),
            ("installed", "Install date"), ("category", "Category"),
            ("behind", "Versions behind"))
        # Only sorts the registry data can actually support: there is no
        # category for 5,576 of the 5,578 plugins, so offering one would be a
        # menu item that silently does nothing.
        self.SORTS_DISCOVER = (
            ("default", "Best match (default)"), ("popular", "Most popular"),
            ("trending", "Trending"), ("newest", "Recently added"),
            ("name", "Name A → Z"), ("name_desc", "Name Z → A"))

        self.sort_btn = Gtk.MenuButton(icon_name="view-sort-descending-symbolic")
        self._rebuild_sort_menu(self.SORTS_INSTALLED)
        self.sort_btn.set_tooltip_text("Sort by")
        header.pack_end(self.sort_btn)

        # The store has no second button: the same control in the same place
        # opens a sort menu on the package list and a filter drawer in the
        # store. Two adjacent buttons that both mean "options" is the thing
        # this is avoiding.
        self.filter_btn = self.sort_btn

        sort_action = Gio.SimpleAction.new_stateful(
            "sort", GLib.VariantType.new("s"), GLib.Variant.new_string("default"))
        sort_action.connect("activate", self._on_sort)
        self.add_action(sort_action)

        menu_btn = Gtk.MenuButton(icon_name="open-menu-symbolic")
        menu_btn.set_menu_model(menu)
        menu_btn.set_tooltip_text("More actions")
        header.pack_end(menu_btn)

        for name, fn in (("install", self.on_install), ("track", self.on_add_appimage),
                         ("fromlink", self.on_install_link),
                         ("clean", self.on_clean_orphans),
                         ("prefs", self.on_preferences),
                         ("selfupdate", self.on_self_update),
                         ("about", self.on_about),
                         ("rescan", lambda *_: self._refresh_page())):
            act = Gio.SimpleAction.new(name, None)
            act.connect("activate", lambda _a, _p, f=fn: f(None))
            self.add_action(act)

        toolbar.add_top_bar(header)

        # One search box for the whole window, in one place on every page,
        # rather than a title-bar widget that silently does nothing on two of
        # the four pages.
        self.search_bar = Gtk.Box(margin_start=12, margin_end=12,
                                  margin_top=6, margin_bottom=2)
        self.search_bar.append(self.search)
        toolbar.add_top_bar(self.search_bar)

        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)

        # The way back from the repo/AUR search, which the sidebar cannot
        # offer: App Store is still the selected row, so re-selecting it
        # emits nothing.
        self.store_back = Gtk.Button(label="Back to the App Store",
                                     icon_name="go-previous-symbolic",
                                     halign=Gtk.Align.START,
                                     margin_start=12, margin_bottom=6)
        self.store_back.add_css_class("flat")
        self.store_back.set_tooltip_text("Return to the plugin marketplace")
        self.store_back.set_visible(False)
        self.store_back.connect("clicked", lambda *_: self._store_return())
        box.append(self.store_back)

        self.view = Gtk.ColumnView(model=selection)
        self.view.set_single_click_activate(True)
        self.view.connect("activate", self._on_row_activated)
        self.view.set_show_row_separators(True)
        self.view.set_show_column_separators(False)
        self._build_columns()
        # Clicking a column header drives the sort.
        # A MultiSorter rather than swapping sorters: with no column selected
        # the column sorter reports every pair equal, so the priority sorter
        # decides; once a header is clicked it takes precedence and priority
        # becomes the tiebreak. Swapping on "changed" instead fired during
        # construction and silently replaced the default ordering.
        self._priority_sorter = Gtk.CustomSorter.new(
            lambda a, b, _: ((a.priority, a.name.lower()) > (b.priority, b.name.lower()))
            - ((a.priority, a.name.lower()) < (b.priority, b.name.lower())), None)
        self._manual_sorter = None
        self._rebuild_sorter()

        scroller = Gtk.ScrolledWindow(vexpand=True)
        scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroller.set_child(self.view)
        scroller.set_margin_start(12)
        scroller.set_margin_end(12)
        scroller.set_margin_bottom(12)

        self.list_stack = Gtk.Stack()
        self.list_stack.set_vexpand(True)
        self.list_stack.add_named(scroller, "list")
        self.list_empty = self._status_page(
            "emblem-ok-symbolic", "Nothing here", "")
        self.list_stack.add_named(self.list_empty, "empty")
        box.append(self.list_stack)

        self.content_stack = Gtk.Stack()
        self.content_stack.set_hhomogeneous(False)
        self.content_stack.set_vhomogeneous(False)
        self.content_stack.set_transition_type(
            Gtk.StackTransitionType.CROSSFADE)
        self.content_stack.add_named(box, "list")
        self.content_stack.add_named(self._build_store(), "store")
        self.content_stack.add_named(self._build_maintenance(), "maintenance")
        self.content_stack.add_named(self._build_history(), "history")
        self.content_stack.add_named(self._build_ai(), "ai")
        self.content_stack.add_named(self._build_featured(), "featured")
        self.content_stack.set_visible_child_name("list")

        # Results and failures appear as toasts. They used to go to whichever
        # of four dim status labels the page happened to own, which is where
        # "Could not remove X" went to die.
        self.toasts = Adw.ToastOverlay()
        self.toasts.set_child(self.content_stack)
        toolbar.set_content(self.toasts)

        split = Adw.OverlaySplitView()
        split.set_sidebar(sidebar_page)
        split.set_content(toolbar)
        split.set_sidebar_width_fraction(0.24)
        split.set_min_sidebar_width(205)
        split.set_max_sidebar_width(260)
        self.split = split
        self.set_content(split)

        # Below ~760px the sidebar costs more than it gives: 205 of 480 pixels
        # is most of a half-screen tile. It folds into an overlay instead, and
        # the toggle in the header becomes the way back to it.
        breakpoint_ = Adw.Breakpoint.new(
            Adw.BreakpointCondition.parse("max-width: 760px"))
        breakpoint_.add_setter(split, "collapsed", True)
        breakpoint_.connect("apply", lambda *_: self._set_narrow(True))
        breakpoint_.connect("unapply", lambda *_: self._set_narrow(False))
        self.add_breakpoint(breakpoint_)
        self._narrow = False

        # Row-menu actions resolve by name at activation time, because the
        # row widgets themselves are recycled as the list scrolls.
        self._row_actions = {}
        self._discovering = False
        self._discover_timer = None
        # Marketplace state. Images are fetched on a small pool -- 5,600 cards
        # would otherwise open 5,600 connections on the first scroll -- and
        # decoded textures are kept so scrolling back is free.
        self._plugins = []
        self._store_cats = [""]
        self._store_cat_key = ""
        self._store_timer = None
        self._store_loaded_once = False
        self._pinning = False
        self._findings = []
        self._maint_loaded_once = False
        self._ai_survey = []
        self._ai_loaded_once = False
        self._shelves = []
        self._feat_loaded_once = False
        # Findings already announced, so a rescan does not re-notify.
        self._reported = set()
        group = Gio.SimpleActionGroup()
        for act, handler in (("visit", self._row_visit), ("remove", self._row_remove),
                             ("notes", self._row_notes), ("aur", self._row_aur),
                             ("folder", self._row_folder),
                             ("changes", self._row_changes),
                             ("pull", self._row_pull),
                             ("stash", self._row_stash)):
            a = Gio.SimpleAction.new(act, GLib.VariantType.new("s"))
            a.connect("activate", lambda _a, param, h=handler: h(param.get_string()))
            group.add_action(a)
        self.insert_action_group("row", group)

        self._apply_all_group_states()
        self._install_shortcuts()
        self._restore_state()
        # Land on a category, always: an unselected sidebar shows an empty
        # list that reads as a bug.
        if self.sidebar_rows and not any(
                r.is_selected() for r in self.sidebar_rows.values()):
            self._select_category("featured" if self.filter_key == "all"
                                  else self.filter_key)
        self._watch_pacman()
        self._watch_configs()
        self.reload()

    # ---- columns ---------------------------------------------------------
    def _column(self, title, setup, bind, sorter, *, expand=False, fixed=0,
                resizable=True):
        factory = Gtk.SignalListItemFactory()
        factory.connect("setup", setup)
        factory.connect("bind", bind)
        col = Gtk.ColumnViewColumn(title=title, factory=factory)
        col.set_expand(expand)
        if fixed:
            # fixed_width is the STARTING width; resizable lets the user drag
            # the header edge from there.
            col.set_fixed_width(fixed)
        col.set_resizable(resizable)
        if sorter:
            col.set_sorter(sorter)
        self.view.append_column(col)
        return col

    def _build_columns(self):
        def num_sorter(attr):
            def cmp(a, b, _):
                x, y = getattr(a, attr), getattr(b, attr)
                return (x > y) - (x < y)
            return Gtk.CustomSorter.new(cmp, None)

        def str_sorter(attr):
            def cmp(a, b, _):
                x, y = str(getattr(a, attr)).lower(), str(getattr(b, attr)).lower()
                # Blanks last in either direction: an empty category is missing
                # data, not a value that deserves the top of the list.
                if not x and y:
                    return 1
                if x and not y:
                    return -1
                return (x > y) - (x < y)
            return Gtk.CustomSorter.new(cmp, None)

        # --- Name: icon + name + description ---
        def setup_name(_f, item):
            row = Gtk.Box(spacing=8, margin_top=4, margin_bottom=4)
            img = Gtk.Image()
            img.set_pixel_size(32)
            img.set_valign(Gtk.Align.CENTER)
            text = Gtk.Box(orientation=Gtk.Orientation.VERTICAL,
                           valign=Gtk.Align.CENTER, hexpand=True)
            title_row = Gtk.Box(spacing=6)
            name = Gtk.Label(xalign=0, hexpand=True)
            name.set_ellipsize(Pango.EllipsizeMode.END)
            pill = Gtk.Label(hexpand=False)
            pill.set_halign(Gtk.Align.START)
            pill.add_css_class("pill")
            pill.set_valign(Gtk.Align.CENTER)
            pill.set_visible(False)
            title_row.append(name)
            title_row.append(pill)
            desc = Gtk.Label(xalign=0)
            desc.add_css_class("dim-label")
            desc.add_css_class("caption")
            desc.set_ellipsize(Pango.EllipsizeMode.END)
            text.append(title_row)
            text.append(desc)
            row.append(img)
            row.append(text)
            item.set_child(row)

        def bind_name(_f, item):
            it = item.get_item()
            img = item.get_child().get_first_child()
            text = img.get_next_sibling()
            title_row = text.get_first_child()
            desc_lbl = title_row.get_next_sibling()
            name_lbl = title_row.get_first_child()
            pill = name_lbl.get_next_sibling()

            name_lbl.set_text(it.name)
            bits = [b for b in (
                it.category,
                _fmt_size(it.size_mb),
                f"installed {_age(it.installed_ts)}" if it.installed_ts else "",
            ) if b]
            desc = (it.app.description or "")[:70]
            meta = " · ".join(bits)
            desc_lbl.set_text(f"{desc}  —  {meta}" if desc and meta else (desc or meta))

            # Only ever shown on a row that actually has an update pending --
            # labelling an up-to-date browser "security" would be noise.
            for css in ("pill-crit", "pill-warn", "pill-ok"):
                pill.remove_css_class(css)
            if it.git_dirty:
                pill.set_text("local edits")
                pill.add_css_class("pill-warn")
                pill.set_tooltip_text(
                    "This checkout has uncommitted changes. Pulling upstream "
                    "would overwrite them — commit or stash first.")
                pill.set_visible(True)
            elif it.source == inventory.SYSTEM and it.has_update:
                pill.set_text("system")
                pill.add_css_class("pill-warn")
                pill.set_tooltip_text(
                    "Omarchy itself — update with the whole system "
                    "(Update all), never on its own")
                pill.set_visible(True)
            elif it.source == inventory.ORPHAN:
                keep = it.advice == "keep"
                pill.set_text("keep" if keep else "safe to remove")
                pill.add_css_class("pill-warn" if keep else "pill-warn")
                pill.set_tooltip_text(
                    "Build tooling or a debug package — the next AUR build will "
                    "want it back" if keep else "Nothing requires this any more")
                pill.set_visible(True)
            elif it.stale and not it.critical:
                label, tip = {
                    "orphaned": ("unmaintained",
                                 "No maintainer in the AUR — nobody is packaging this any more"),
                    "flagged": ("out of date",
                                "The AUR maintainer has flagged this as out of date"),
                    "stale": ("stale",
                              f"No new build in over {inventory.STALE_DAYS // 30} months — "
                              "upstream may have stopped"),
                }[it.stale]
                pill.set_text(label)
                pill.add_css_class("pill-warn")
                pill.set_tooltip_text(tip)
                pill.set_visible(True)
            elif it.critical == "security":
                pill.set_text("security")
                pill.set_tooltip_text(
                    "Network-facing or credential-handling: a stale version is exposure")
                pill.set_visible(True)
            elif it.critical == "core":
                pill.set_text("core")
                pill.add_css_class("pill-warn")
                pill.set_tooltip_text(
                    "Touches boot or needs a reboot — update with the full system, not alone")
                pill.set_visible(True)
            else:
                pill.set_visible(False)

            img.remove_css_class("dim-label")
            if not self._app_icon(img, it.app, 32):
                img.add_css_class("dim-label")

        self._column("Name", setup_name, bind_name, str_sorter("name"), expand=True)

        # --- plain text columns ---
        def make_text(getter, dim=True, align=0.0, accent_on_update=False):
            def setup(_f, item):
                lbl = Gtk.Label(xalign=align)
                lbl.set_ellipsize(Pango.EllipsizeMode.END)
                lbl.add_css_class("caption")
                item.set_child(lbl)

            def bind(_f, item):
                lbl, it = item.get_child(), item.get_item()
                lbl.set_text(getter(it))
                lbl.remove_css_class("accent")
                lbl.remove_css_class("dim-label")
                if accent_on_update and it.has_update:
                    # The one colour in the table: a pending update is the most
                    # actionable fact in the row.
                    lbl.add_css_class("accent")
                elif dim:
                    lbl.add_css_class("dim-label")
            return setup, bind

        s, b = make_text(_version_text, dim=True, align=1.0, accent_on_update=True)
        self._column("Version", s, b, num_sorter("behind"), fixed=170)

        def setup_source(_f, item):
            # A LinkButton would underline permanently and steal the row's
            # styling; a flat button reads as a label until hovered.
            btn = Gtk.Button()
            btn.add_css_class("flat")
            btn.add_css_class("caption")
            btn.set_halign(Gtk.Align.START)
            btn.set_valign(Gtk.Align.CENTER)
            item.set_child(btn)

        def bind_source(_f, item):
            btn, it = item.get_child(), item.get_item()
            label = SOURCE_LABEL.get(it.source, it.source)
            url = it.app.extra.get("url", "")
            btn.set_label(label)
            if getattr(btn, "_handler", None):
                btn.disconnect(btn._handler)
                btn._handler = None
            if url:
                host = url.split("//")[-1].split("/")[0]
                btn.set_tooltip_text(f"Open {url}")
                btn.set_sensitive(True)
                btn.add_css_class("link-ish")
                btn._handler = btn.connect(
                    "clicked", lambda _b, u=url: self.open_url(u))
            else:
                # Local builds and AppImages often have nowhere to point.
                btn.set_tooltip_text("No upstream URL recorded")
                btn.set_sensitive(False)
                btn.remove_css_class("link-ish")

        self._column("Source", setup_source, bind_source,
                     str_sorter("source"), fixed=74)

        # --- launch button ---
        def setup_act(_f, item):
            row = Gtk.Box(spacing=2, halign=Gtk.Align.END,
                          valign=Gtk.Align.CENTER)
            upd = Gtk.Button(icon_name="software-update-available-symbolic")
            upd.add_css_class("flat")
            upd.add_css_class("circular")
            play = Gtk.Button(icon_name="media-playback-start-symbolic")
            play.add_css_class("flat")
            menu = Gtk.MenuButton(icon_name="view-more-symbolic")
            menu.add_css_class("flat")
            row.append(upd)
            row.append(play)
            row.append(menu)
            item.set_child(row)

        def bind_act(_f, item):
            it = item.get_item()
            row = item.get_child()
            upd = row.get_first_child()
            play = upd.get_next_sibling()
            menu = play.get_next_sibling()

            # Rows are recycled as the list scrolls, so a stale handler would
            # fire for whichever item later lands in this widget.
            for btn in (upd, play):
                if getattr(btn, "_handler", None):
                    btn.disconnect(btn._handler)
                    btn._handler = None

            if it.source in (inventory.AVAILABLE, inventory.PLUGIN):
                already = it.app.extra.get("already")
                upd.set_visible(not already)
                play.set_visible(False)
                menu.set_visible(bool(it.app.extra.get("url")))
                if not already:
                    upd.remove_css_class("error")
                    upd.add_css_class("accent")
                    upd.set_icon_name(INSTALL_ICON)
                    upd.set_tooltip_text(
                        f"Install {it.name}"
                        + (" (Omarchy plugin)" if it.source == inventory.PLUGIN else ""))
                    upd._handler = upd.connect("clicked", self.on_install_item, it.app)
                if it.app.extra.get("url"):
                    m = Gio.Menu()
                    m.append("Open repository", f"row.visit::{it.name}")
                    menu.set_menu_model(m)
                    self._row_actions[it.name] = it.app
                return

            upd.set_icon_name("software-update-available-symbolic")
            can_update = it.has_update and it.source in (
                inventory.REPO, inventory.AUR, inventory.APPIMAGE,
                inventory.TOOL)
            if it.source == inventory.SYSTEM:
                # Almanac is a SYSTEM entry but is the one thing here that
                # can genuinely update itself.
                can_update = bool(it.app.extra.get("self")) and it.has_update
            elif it.source == inventory.LOCAL:
                # Never offer a pull over uncommitted work.
                can_update = bool(it.git_behind) and not it.git_dirty
            upd.set_visible(can_update)
            if can_update:
                upd.set_tooltip_text(
                    f"Update {it.name} alone, in a terminal.\n"
                    "Arch discourages partial upgrades: prefer Update all "
                    "unless you need just this one.")
                # Colour only where it earns attention: a security update is
                # worth a red glyph, routine ones stay quiet.
                upd.remove_css_class("error")
                upd.remove_css_class("accent")
                upd.add_css_class("error" if it.critical == "security" else "accent")
                if it.app.extra.get("self"):
                    upd.set_tooltip_text(
                        f"Update Almanac itself — {it.app.extra.get('note', '')}")
                    upd._handler = upd.connect(
                        "clicked", lambda _b: self.on_self_update())
                elif it.source == inventory.LOCAL:
                    upd.set_tooltip_text(
                        f"git pull in {it.app.extra.get('repo', '')}\n"
                        f"Latest upstream: {it.app.extra.get('git_subject', '')}")
                    upd._handler = upd.connect("clicked", self.on_git_pull, it.app)
                elif it.source == inventory.APPIMAGE:
                    upd.set_tooltip_text(
                        f"Download {it.name} {it.app.update} and replace the "
                        "current build (the old one is kept as .previous)")
                    upd._handler = upd.connect(
                        "clicked", self.on_update_appimage, it.app)
                else:
                    upd._handler = upd.connect("clicked", self.on_update_one, it.app)

            missing = (it.source == inventory.APPIMAGE
                       and not it.app.extra.get("present", True))
            launchable = bool(it.app.exec_path) and it.source in (
                inventory.APPIMAGE, inventory.LOCAL)
            play.set_visible(launchable and not missing)
            if play.get_visible():
                play.set_tooltip_text(f"Launch {it.name}")
                play._handler = play.connect("clicked", self.on_launch, it.app)

            # Destructive actions sit behind a menu, never a bare button where
            # the pointer already is.
            model = Gio.Menu()
            url = it.app.extra.get("url", "")
            if url:
                model.append("Open project page", f"row.visit::{it.name}")
            if it.has_update and "github.com/" in url:
                model.append("What’s new…", f"row.notes::{it.name}")
            elif it.source == inventory.AUR:
                model.append("View on the AUR", f"row.aur::{it.name}")
            if it.source in (inventory.REPO, inventory.AUR,
                             inventory.APPIMAGE, inventory.FLATPAK):
                model.append("Remove…", f"row.remove::{it.name}")

            # A local checkout with uncommitted work gets no pull button,
            # because the pull would be refused. Without these it got no
            # affordance at all and no explanation -- a row 209 commits
            # behind that simply does nothing.
            if it.source == inventory.LOCAL and it.app.extra.get("repo"):
                model.append("Open repository folder",
                             f"row.folder::{it.name}")
                if it.git_dirty:
                    model.append("Show local changes…",
                                 f"row.changes::{it.name}")
                    if it.git_behind:
                        model.append("Stash changes and update…",
                                     f"row.stash::{it.name}")
                elif it.git_behind:
                    model.append("Update (git pull)", f"row.pull::{it.name}")

            menu.set_menu_model(model if model.get_n_items() else None)
            menu.set_visible(model.get_n_items() > 0)
            self._row_actions[it.name] = it.app

        self._column("", setup_act, bind_act, None, fixed=104, resizable=False)

    # ---- shortcuts and persisted state -----------------------------------
    def _install_shortcuts(self):
        controller = Gtk.ShortcutController()
        controller.set_scope(Gtk.ShortcutScope.GLOBAL)

        def add(accel, fn):
            controller.add_shortcut(Gtk.Shortcut(
                trigger=Gtk.ShortcutTrigger.parse_string(accel),
                action=Gtk.CallbackAction.new(lambda *_: (fn(), True)[1])))

        add("<Control>f", lambda: self.search.grab_focus())
        # Refresh means "re-read this page", the same as the menu item. It
        # used to always re-scan packages, whatever you were looking at.
        add("<Control>r", self._refresh_page)
        add("F5", self._refresh_page)
        add("<Control>u", lambda: self.on_update_all(None))
        add("<Control>w", self.close)
        add("Escape", self._clear_search)

        # The primary destinations are reachable without the mouse, in the
        # order they appear in the sidebar.
        for i, key in enumerate(("featured", "discover", "themes", "updates",
                                 inventory.AGENT, "apps", "all",
                                 "maintenance", "history"), start=1):
            add(f"<Control>{i}", lambda k=key: self._select_category(k))
        self.add_controller(controller)

    def _clear_search(self):
        if self.search.get_text():
            self.search.set_text("")
        else:
            self.set_focus(None)

    @property
    def _state_path(self) -> Path:
        return Path(inventory.REGISTRY).parent / "window-state.json"

    def _restore_state(self):
        """Window size, chip and sort survive a restart -- otherwise every
        launch dumps you back on an unsorted "All"."""
        try:
            st = json.loads(self._state_path.read_text())
        except (OSError, ValueError):
            return
        w, h = st.get("width"), st.get("height")
        if isinstance(w, int) and isinstance(h, int) and w > 400 and h > 300:
            self.set_default_size(w, h)
        if isinstance(st.get("collapsed"), list):
            self._collapsed = {g for g in st["collapsed"] if g in self.group_rows}
            self._apply_all_group_states()
        chip = st.get("chip")
        if chip in self.sidebar_rows:
            self._select_category(chip)
        self._wanted_sort = (st.get("sort_col"), st.get("sort_desc", False))

    def _save_state(self):
        col_title, desc = "", False
        sorter = self.view.get_sorter()
        if sorter and (c := sorter.get_primary_sort_column()):
            col_title = c.get_title() or ""
            desc = sorter.get_primary_sort_order() == Gtk.SortType.DESCENDING
        try:
            self._state_path.write_text(json.dumps({
                "width": self.get_width(), "height": self.get_height(),
                "chip": self.filter_key, "sort_col": col_title, "sort_desc": desc,
                "collapsed": sorted(self._collapsed),
            }, indent=2))
        except OSError:
            pass                        # losing window state is not worth an error

    # ---- external change detection ---------------------------------------
    def _watch_pacman(self):
        """Rescan when the package database changes, whoever changed it.

        pacman.log gets one line per transaction, so a single upgrade fires
        many events -- hence the debounce. Keeping a reference to the monitor
        matters: a GC'd FileMonitor silently stops delivering.
        """
        self._pending_rescan = None
        try:
            gfile = Gio.File.new_for_path("/var/log/pacman.log")
            self._monitor = gfile.monitor_file(Gio.FileMonitorFlags.NONE, None)
            self._monitor.connect("changed", self._on_pacman_changed)
        except GLib.Error:
            self._monitor = None        # no watch is survivable; Rescan still works

    def _watch_configs(self):
        """Notice the moment a tool resets a config file to its defaults.

        The Health check finds a reset whenever it runs; this makes it
        immediate while the window is open. Only the directories the
        distribution ships defaults for are watched, since nothing else can
        be "reset to defaults".
        """
        self._config_monitors = []
        self._config_pending = None
        self._config_reported = {str(r.backup) for r in guard.resets()}
        root = guard.defaults_root()
        if root is None:
            return
        for sub in sorted(root.iterdir()) if root.is_dir() else []:
            target = guard.config_home() / sub.name
            if not target.is_dir():
                continue
            try:
                mon = Gio.File.new_for_path(str(target)).monitor_directory(
                    Gio.FileMonitorFlags.NONE, None)
            except GLib.Error:
                continue
            mon.connect("changed", self._on_config_changed)
            self._config_monitors.append(mon)

    def _on_config_changed(self, _mon, gfile, _other, event):
        if event != Gio.FileMonitorEvent.CREATED:
            return
        if not guard.BAK.match(gfile.get_basename() or ""):
            return
        if self._config_pending:
            GLib.source_remove(self._config_pending)
        # The backup is written before the defaults are copied in; wait for both.
        self._config_pending = GLib.timeout_add_seconds(3, self._config_check)

    def _config_check(self):
        self._config_pending = None

        def work():
            found = guard.resets()
            GLib.idle_add(self._config_report, found)
        threading.Thread(target=work, daemon=True).start()
        return False

    def _config_report(self, found):
        new = [r for r in found if str(r.backup) not in self._config_reported]
        self._config_reported |= {str(r.backup) for r in found}
        if not new:
            return False
        names = ", ".join(r.name for r in new)
        lost = sum(len(r.lost) for r in new)
        body = (f"{names} was replaced with the shipped defaults"
                + (f", removing {lost} of your entries" if lost else "")
                + ". Your version was kept — restore it from Health check.")
        self._notify("Your settings were reset", body, "critical")
        self._say("Settings reset to defaults — see Health check", 10)
        if self.content_stack.get_visible_child_name() == "maintenance":
            self._maint_reload()
        return False

    def _on_pacman_changed(self, *_args):
        if self._pending_rescan:
            GLib.source_remove(self._pending_rescan)
        # Wait for the transaction to settle, then rescan once.
        self._pending_rescan = GLib.timeout_add_seconds(6, self._rescan_now)

    def _rescan_now(self):
        self._pending_rescan = None
        # Logged as well as shown, so the watch can be verified from a terminal
        # rather than only by staring at the window.
        print("pacman.log changed — rescanning", flush=True)
        self._summarize("Packages changed — rescanning…")
        self.reload()
        return False                    # one-shot

    # ---- data ------------------------------------------------------------
    def reload(self):
        if (act := self.lookup_action("rescan")):
            act.set_enabled(False)
        self._summarize("Scanning…")

        def work():
            apps = inventory.collect()
            GLib.idle_add(self._apply, apps)

        # checkupdates and yay take seconds; on the UI thread that is a freeze.
        threading.Thread(target=work, daemon=True).start()

    def _notify(self, title: str, body: str, urgency: str = "normal"):
        """Desktop notification, falling back to the status line."""
        tool = shutil.which("omarchy-notification-send") or shutil.which("notify-send")
        if not tool:
            self._say(f"{title}: {body}")
            return
        try:
            subprocess.Popen([tool, "-u", urgency, title, body],
                             start_new_session=True)
        except OSError:
            self._say(f"{title}: {body}")

    def _report_health(self, apps):
        """Tell the user once about things that need a decision, not a click.

        Only new findings are announced: a notification repeated on every
        rescan is one the user learns to dismiss without reading.
        """
        abandoned = [a for a in apps
                     if a.extra.get("stale") in ("orphaned", "flagged")]
        dupes = inventory.duplicates(apps)

        findings = set()
        for a in abandoned:
            findings.add(f"stale:{a.name}:{a.extra['stale']}")
        for k in dupes:
            findings.add(f"dupe:{k}")

        new = findings - self._reported
        self._reported |= findings
        if not new:
            return

        if abandoned and any(f.startswith("stale:") for f in new):
            lines = [f"{a.name} — "
                     + ("no maintainer in the AUR" if a.extra["stale"] == "orphaned"
                        else "flagged out of date by its maintainer")
                     for a in abandoned]
            self._notify("Software no longer maintained",
                         "\n".join(lines[:5]), "critical")

        if dupes and any(f.startswith("dupe:") for f in new):
            lines = [f"{k}: {', '.join(v)}" for k, v in list(dupes.items())[:5]]
            self._notify("Duplicate software installed", "\n".join(lines))

    def _apply(self, apps):
        self.store.remove_all()
        for a in apps:
            self.store.append(AppItem(a))
        if (act := self.lookup_action("rescan")):
            act.set_enabled(True)
        self.filter.changed(Gtk.FilterChange.DIFFERENT)
        self._update_status()
        self._report_health(apps)
        return False

    def _update_status(self):
        total = self.store.get_n_items()
        updates = sum(1 for i in range(total) if self.store.get_item(i).has_update)
        shown = self.sorted.get_n_items()
        crit = sum(1 for i in range(total) if self.store.get_item(i).critical)

        orphan_n = sum(1 for i in range(total)
                       if self.store.get_item(i).source == inventory.ORPHAN)
        installed = total - orphan_n
        text = f"{installed} installed · {updates} with updates"
        if crit:
            text += f" · {crit} critical"
        if orphan_n:
            text += f" · {orphan_n} orphans"
        if shown != total:
            text += f" · showing {shown}"
        # Say which kind of empty this is, rather than showing a blank table.
        if shown == 0:
            icon, title, body = self.EMPTY_STATES.get(
                self.filter_key,
                ("system-search-symbolic", "Nothing matches",
                 "Try a different search."))
            if self.query:
                icon, title = "system-search-symbolic", "Nothing matches"
                body = f"No installed software matches “{self.query}”."
            self.list_empty.set_icon_name(icon)
            self.list_empty.set_title(title)
            self.list_empty.set_description(body)
            self.list_stack.set_visible_child_name("empty")
        else:
            self.list_stack.set_visible_child_name("list")

        self._summarize(text, "packages")
        self._sync_primary()
        self._update_sidebar_counts()
        self._refresh_stats()
        removable = sum(1 for i in range(total)
                        if self.store.get_item(i).source == inventory.ORPHAN
                        and self.store.get_item(i).advice != "keep")
        if (act := self.lookup_action("clean")):
            act.set_enabled(removable > 0)
        self.update_btn.set_label(f"Update all ({updates})" if updates else "Update all")

    def _on_row_activated(self, _view, position):
        item = self.sorted.get_item(position)
        if item is not None:
            self._show_details(item)

    def _show_details(self, item):
        """A card for one app: everything known about it, and what can be done.

        Details are fetched on open rather than during the scan -- dependency
        lists are a pacman call each, which would make the list slow for data
        nobody reads until they ask for it.
        """
        app = item.app
        extra = inventory.details(app)

        page = Adw.PreferencesPage()

        # --- header: icon, name, what it is ---
        hero = Adw.PreferencesGroup()
        head = Gtk.Box(spacing=12, margin_bottom=4)
        img = Gtk.Image()
        img.set_pixel_size(64)
        self._app_icon(img, item.app, 128)
        head.append(img)

        titles = Gtk.Box(orientation=Gtk.Orientation.VERTICAL,
                         valign=Gtk.Align.CENTER, spacing=2)
        t = Gtk.Label(xalign=0, label=app.name)
        t.add_css_class("title-2")
        titles.append(t)
        if app.description:
            d = Gtk.Label(xalign=0, label=app.description, wrap=True)
            d.add_css_class("dim-label")
            titles.append(d)
        badges = Gtk.Box(spacing=6, margin_top=4)
        for text, css in self._detail_badges(item):
            b = Gtk.Label(label=text)
            b.add_css_class(css)
            badges.append(b)
        if badges.get_first_child():
            titles.append(badges)
        head.append(titles)
        hero.add(head)
        page.add(hero)

        # --- version ---
        vg = Adw.PreferencesGroup(title="Version")
        self._detail_row(vg, "Installed", app.version)
        if app.update:
            label = app.update
            if item.behind and item.behind_kind in ("major", "minor", "patch", "point"):
                label += f"  ({item.behind} {item.behind_kind}"
                label += "s behind)" if item.behind > 1 else " behind)"
            elif item.behind_kind == "rel":
                label += "  (rebuild only)"
            self._detail_row(vg, "Available", label)
        if extra.get("published"):
            self._detail_row(vg, "Released", extra["published"])
        if extra.get("git_subject"):
            self._detail_row(vg, "Latest upstream commit", extra["git_subject"])
        page.add(vg)

        # Advisories from the Arch Security Tracker, each linked to its page.
        if extra.get("vulns"):
            sg = Adw.PreferencesGroup(
                title="Known vulnerabilities",
                description="From the Arch Security Tracker, for the installed "
                            "version.")
            for v in extra["vulns"]:
                cves = ", ".join(v["cves"][:4]) + ("…" if len(v["cves"]) > 4 else "")
                fix = f"fixed in {v['fixed']}" if v["fixed"] else "no fix released yet"
                row = self._plain(title=f"{v['group']} · {v['severity']} · {v['kind']}",
                                  subtitle=f"{fix} · {cves}")
                row.set_subtitle_lines(2)
                link = Gtk.Button(icon_name="adw-external-link-symbolic",
                                  valign=Gtk.Align.CENTER,
                                  tooltip_text="Open the advisory")
                link.add_css_class("flat")
                link.update_property([Gtk.AccessibleProperty.LABEL],
                                     [f"Open advisory {v['group']}"])
                link.connect("clicked", lambda _b, g=v["group"]:
                             self.open_url(f"https://security.archlinux.org/{g}"))
                row.add_suffix(link)
                sg.add(row)
            page.add(sg)

        # --- where it came from ---
        og = Adw.PreferencesGroup(title="Origin")
        self._detail_row(og, "Source", SOURCE_LABEL.get(item.source, item.source))
        for key, label in (("category", "Category"), ("size", "Installed size"),
                           ("license", "Licence"), ("Packager", "Packager"),
                           ("Architecture", "Architecture"), ("repo", "Checkout")):
            val = extra.get(key) or (item.category if key == "category" else "")
            if val:
                self._detail_row(og, label, str(val))
        if item.installed_ts:
            self._detail_row(og, "Installed",
                             datetime.fromtimestamp(item.installed_ts)
                             .strftime("%d %b %Y, %H:%M"))
        if extra.get("built_ts"):
            self._detail_row(og, "Built",
                             datetime.fromtimestamp(extra["built_ts"])
                             .strftime("%d %b %Y"))
        if extra.get("files"):
            self._detail_row(og, "Files", f"{extra['files']:,}")
        if extra.get("url"):
            r = Adw.ActionRow(title="Project page", subtitle=extra["url"])
            r.set_subtitle_selectable(True)
            btn = Gtk.Button(icon_name="adw-external-link-symbolic", valign=Gtk.Align.CENTER)
            btn.add_css_class("flat")
            btn.connect("clicked", lambda _b, u=extra["url"]: self.open_url(u))
            r.add_suffix(btn)
            og.add(r)
        page.add(og)

        # --- relationships ---
        deps = Adw.PreferencesGroup(title="Dependencies")
        added = False
        for key, label in (("Depends On", "Requires"), ("Required By", "Required by"),
                           ("Optional Deps", "Optional"), ("Provides", "Provides"),
                           ("Conflicts With", "Conflicts with")):
            if extra.get(key):
                row = Adw.ActionRow(title=label,
                                    subtitle=" ".join(extra[key].split())[:400])
                row.set_subtitle_lines(4)
                row.set_subtitle_selectable(True)
                deps.add(row)
                added = True
        if added:
            page.add(deps)

        dialog = Adw.Dialog()
        dialog.set_title(app.name)
        dialog.set_content_width(620)
        dialog.set_content_height(720)

        tv = Adw.ToolbarView()
        hb = Adw.HeaderBar()

        if app.update and item.source not in (inventory.SYSTEM,):
            up = Gtk.Button(label="Update")
            up.add_css_class("suggested-action")
            up.connect("clicked", lambda _b: (dialog.close(), self._update_from_detail(item)))
            hb.pack_end(up)
        tv.add_top_bar(hb)

        scroll = Gtk.ScrolledWindow(vexpand=True)
        scroll.set_child(page)
        tv.set_content(scroll)
        dialog.set_child(tv)
        dialog.present(self)

    def _detail_badges(self, item):
        out = []
        if item.critical == "security":
            out.append(("security update", "pill-crit"))
        elif item.critical == "core":
            out.append(("core system", "pill-crit"))
        if item.stale == "orphaned":
            out.append(("unmaintained", "pill-warn"))
        elif item.stale == "flagged":
            out.append(("flagged out of date", "pill-warn"))
        if item.git_dirty:
            out.append(("local edits", "pill-warn"))
        if item.source == inventory.ORPHAN:
            out.append(("orphan · " + (item.advice or ""), "pill-warn"))
        return out

    @staticmethod
    def _plain(cls=None, title="", subtitle="", **kwargs):
        """A row whose text is taken literally.

        Adw rows parse title and subtitle as Pango markup, and every string
        put in one here was written by someone else -- a skill description, a
        marketplace listing, a package summary. An unescaped "&" or "<name>"
        makes the row render empty.

        The order matters: the text is parsed when it is assigned, so markup
        has to be switched off before the title and subtitle go in, not after.
        """
        row = (cls or Adw.ActionRow)(**kwargs)
        row.set_use_markup(False)
        if title:
            row.set_title(title)
        if subtitle:
            row.set_subtitle(subtitle)
        return row

    @staticmethod
    def _detail_row(group, title, value):
        # A row reading "Reviewed by: (blank)" says less than no row at all,
        # and the provenance group has several fields that only some entries
        # carry.
        if value in (None, ""):
            return
        row = AlmanacWindow._plain(title=title, subtitle=str(value))
        row.set_subtitle_selectable(True)
        group.add(row)

    def _update_from_detail(self, item):
        if item.source == inventory.APPIMAGE:
            self.on_update_appimage(Gtk.Button(), item.app)
        elif item.source == inventory.LOCAL:
            self.on_git_pull(None, item.app)
        else:
            self.on_update_one(None, item.app)

    def _rebuild_sort_menu(self, options):
        menu = Gio.Menu()
        for key, label in options:
            menu.append(label, f"win.sort::{key}")
        self.sort_btn.set_menu_model(menu)

    def _on_sort(self, action, param):
        """Sort by a field that is no longer its own column.

        Selecting one replaces the column sorter; clicking a column header
        afterwards takes precedence again, because the MultiSorter still has
        the view's sorter first.
        """
        key = param.get_string()
        action.set_state(param)
        getters = {
            "name": lambda i: i.name.lower(),
            "name_desc": lambda i: _reverse_text(i.name.lower()),
            "size": lambda i: -i.size_mb,
            "installed": lambda i: -i.installed_ts,
            "category": lambda i: (i.category == "", i.category.lower()),
            "behind": lambda i: -i.behind,
            # All-time standing. Unverified plugins sink: anyone can list one.
            "popular": lambda i: (-i.votes, not i.verified, i.name.lower()),
            # The AUR's popularity is already decay-weighted, which is what
            # "trending" means; plugins fall back to how recently they landed.
            "trending": lambda i: (-i.popularity, i.added and -int(
                i.added.replace("-", "") or 0), i.name.lower()),
            "newest": lambda i: (i.added == "", _reverse_text(i.added),
                                 i.name.lower()),
        }
        if key == "default":
            self._manual_sorter = None
        else:
            g = getters[key]
            self._manual_sorter = Gtk.CustomSorter.new(
                lambda a, b, _, g=g: (g(a) > g(b)) - (g(a) < g(b)), None)
        self._rebuild_sorter()

    def _rebuild_sorter(self):
        combined = Gtk.MultiSorter()
        combined.append(self.view.get_sorter())     # a clicked header wins
        if getattr(self, "_manual_sorter", None):
            combined.append(self._manual_sorter)
        combined.append(self._priority_sorter)      # then urgency, then name
        self.sorted.set_sorter(combined)

    @staticmethod
    def _badge(label, count: int, level: str = ""):
        """Set a sidebar count and how loudly it should read.

        Three levels, and only one of them is red. Every badge used to turn
        red as soon as any single package anywhere was security-critical,
        which painted Featured, Updates and AI the same alarming colour and
        left the actual security item indistinguishable from the rest.
        """
        label.set_text(str(count) if count else "")
        for css in ("is-warn", "is-crit"):
            label.remove_css_class(css)
        label.set_visible(bool(count))
        if not count:
            return
        if level == "crit":
            label.add_css_class("is-crit")
        elif level == "warn":
            label.add_css_class("is-warn")

    def _stat_row(self, label: str, value: str, tooltip: str = ""):
        row = Gtk.Box(spacing=6)
        name = Gtk.Label(label=label, xalign=0, hexpand=True)
        name.add_css_class("stat-key")
        val = Gtk.Label(label=value, xalign=1)
        val.add_css_class("stat-val")
        row.append(name)
        row.append(val)
        if tooltip:
            row.set_tooltip_text(tooltip)
        return row

    def _refresh_stats(self):
        """The numbers you would otherwise open a terminal to get."""
        while (child := self.stats.get_first_child()) is not None:
            self.stats.remove(child)

        items = [self.store.get_item(i) for i in range(self.store.get_n_items())]
        total = sum(1 for i in items if i.source != inventory.ORPHAN)
        size = sum(inventory.size_mb(i.app.extra.get("size", "")) for i in items)
        explicit = sum(1 for i in items if i.source in (inventory.REPO,
                                                        inventory.AUR))
        foreign = sum(1 for i in items if i.source == inventory.AUR)
        # Counting packages, not releases: summing release deltas across
        # date-versioned and epoch-versioned packages produced numbers in the
        # tens of thousands, which described nothing.
        majors = sum(1 for i in items if i.behind and i.behind_kind == "major")
        pending = sum(1 for i in items if i.has_update)
        security = sum(1 for i in items if i.critical == "security")

        rows = [("Installed", f"{total}", "Everything Almanac can see, orphans aside"),
                ("On disk", maintenance.fmt_size(size),
                 "Installed size reported by the package manager"),
                ("Updates", f"{pending}",
                 "Packages, AppImages, plugins and agents with a newer version"),
                ("From the AUR", f"{foreign}",
                 f"{foreign} of {explicit} repo and AUR packages are foreign")]
        if majors:
            rows.append(("A major behind", f"{majors}",
                         "Packages at least one major version behind upstream"))
        if security:
            rows.append(("Security", f"{security}",
                         "Network-facing packages with an update waiting"))

        free = maintenance.reclaimable_mb(getattr(self, "_findings", []) or [])
        if free:
            rows.append(("Reclaimable", maintenance.fmt_size(free),
                         "Caches and logs that can be cleared"))

        kernel = self._kernel_version()
        if kernel:
            rows.append(("Kernel", kernel, "Running kernel release"))

        up = self._uptime()
        if up:
            rows.append(("Uptime", up, "Since last boot"))

        for label, value, tip in rows:
            self.stats.append(self._stat_row(label, value, tip))

    @staticmethod
    def _kernel_version() -> str:
        try:
            return os.uname().release.split("-arch")[0]
        except Exception:                               # noqa: BLE001
            return ""

    @staticmethod
    def _uptime() -> str:
        import demo
        if demo.active():
            return "6d 2h"
        try:
            with open("/proc/uptime") as f:
                seconds = float(f.read().split()[0])
        except (OSError, ValueError):
            return ""
        days, rem = divmod(int(seconds), 86400)
        hours, minutes = divmod(rem // 60, 60)
        if days:
            return f"{days}d {hours}h"
        if hours:
            return f"{hours}h {minutes}m"
        return f"{minutes}m"

    def _update_sidebar_counts(self):
        """Per-category totals, so the sidebar answers "what needs me?" at rest."""
        items = [self.store.get_item(i) for i in range(self.store.get_n_items())]
        counts = {
            "updates": sum(1 for i in items if i.has_update),
            "apps": sum(1 for i in items
                        if (i.app.extra.get("is_app")
                            or i.source in (inventory.APPIMAGE, inventory.LOCAL))
                        and i.source != inventory.ORPHAN),
            "all": sum(1 for i in items if i.source != inventory.ORPHAN),
        }
        for src in (inventory.SYSTEM, inventory.REPO, inventory.AUR,
                    inventory.APPIMAGE, inventory.FLATPAK, inventory.LOCAL,
                    inventory.AGENT, inventory.TOOL, inventory.ORPHAN):
            counts[src] = sum(1 for i in items if i.source == src)

        crit = sum(1 for i in items if i.critical)
        for gkey, header in self.group_rows.items():
            if header is None:
                continue
            members = self.group_members.get(gkey, [])
            if gkey in self._collapsed and len(members) > 1:
                total = sum(counts.get(r._key, 0) for r in members)
                header._chevron.set_tooltip_text(f"{total} items")
            else:
                header._chevron.set_tooltip_text(None)
        for key, row in self.sidebar_rows.items():
            if key in ("maintenance", "history", inventory.AGENT, "featured",
                       "themes"):
                continue            # their badges count findings, not packages
            n = counts.get(key, 0)
            # An empty category stays visible but recedes, so the layout does
            # not reshuffle as things are installed and removed.
            # "discover" has no inventory count of its own and "all"/"updates"
            # must stay reachable when empty, so none of the three is gated on
            # a count. Greying out the App Store is what made it unclickable.
            row.set_sensitive(
                n > 0 or key in ("all", "updates", "discover", "themes",
                                 "maintenance", "featured", inventory.AGENT))
            # Red means "a security update is waiting", nothing else. Pending
            # updates are a number, not an emergency.
            security = sum(1 for i in items if i.critical == "security")
            self._badge(row._count, n,
                        "crit" if (key == "updates" and security) else "")

    # ---- filtering -------------------------------------------------------
    def _match(self, item, _data):
        if item.source in (inventory.AVAILABLE, inventory.PLUGIN):
            return True
        if self.filter_key == "updates" and not item.has_update:
            return False
        if self.filter_key == "apps" and not (
                item.app.extra.get("is_app")
                or item.source in (inventory.APPIMAGE, inventory.LOCAL)):
            return False
        if self.filter_key in ("all", "updates", "apps") and item.source == inventory.ORPHAN:
            return False
        if self.filter_key not in ("all", "updates", "apps") and item.source != self.filter_key:
            return False
        if self.query and self.query not in item.haystack:
            return False
        return True

    def _run_discover(self):
        """Query the repos and the AUR for what the search box says.

        Debounced: every keystroke would otherwise be a pacman -Ss plus an AUR
        round trip.
        """
        self._discovering = True
        if self._discover_timer:
            GLib.source_remove(self._discover_timer)
        self._discover_timer = GLib.timeout_add(450, self._discover_now)

    def _discover_now(self):
        self._discover_timer = None
        query = self.search.get_text().strip()
        browsing = len(query) < 2
        self._summarize("Loading the plugin marketplace…" if browsing
                             else f"Searching for “{query}”…")

        def work():
            if browsing:
                # Nothing typed: show the marketplace itself. Packages are
                # excluded here -- 15,669 of them is a database dump, not a
                # storefront, and there is no popularity signal to rank them.
                results = inventory.search_plugins("", limit=200)
            else:
                plugins = inventory.search_plugins(query, limit=25)
                packages = inventory.search_available(query)
                # Plugins lead: on an Omarchy box they are the likelier intent,
                # and the package list is long enough to bury them otherwise.
                results = plugins + packages
            GLib.idle_add(self._show_discover, query, results)

        threading.Thread(target=work, daemon=True).start()
        return False

    def _show_discover(self, query, results):
        if not self._discovering:
            return False                # user navigated away mid-search
        self.store.remove_all()
        for a in results:
            self.store.append(AppItem(a))
        self.filter.changed(Gtk.FilterChange.DIFFERENT)
        if not query:
            total = len(inventory.plugin_registry().get("sources") or [])
            self._summarize(
                f"Showing {len(results)} of {total} Omarchy plugins · "
                "search to include repo and AUR packages")
        else:
            self._summarize(
                f"{len(results)} installable result(s) for “{query}”"
                if results else f"Nothing found for “{query}”")
        self.update_btn.set_sensitive(False)
        return False

    def on_search(self, entry):
        if self.filter_key == "discover":
            if self.content_stack.get_visible_child_name() == "store":
                self._store_queue()
            else:
                self._run_discover()
            return
        self.query = entry.get_text().strip().lower()
        self.filter.changed(Gtk.FilterChange.DIFFERENT)
        self._update_status()

    def _on_sidebar_activated(self, _list, row):
        """Header rows fold their group; everything else is a category."""
        group = getattr(row, "_group", None)
        if group is None or not isinstance(row, Gtk.ListBoxRow) or hasattr(row, "_key"):
            return
        self._toggle_group(group)

    def _toggle_group(self, gkey: str):
        collapsed = gkey in self._collapsed
        if collapsed:
            self._collapsed.discard(gkey)
        else:
            self._collapsed.add(gkey)
        self._apply_group_state(gkey)

    def _apply_group_state(self, gkey: str):
        if self.group_rows.get(gkey) is None:
            return                      # ungrouped primaries never fold
        collapsed = gkey in self._collapsed
        header = self.group_rows.get(gkey)
        if header is not None:
            header._chevron.set_from_icon_name(
                "pan-end-symbolic" if collapsed else "pan-down-symbolic")
        for row in self.group_members.get(gkey, []):
            # The selected category stays visible even when its group folds,
            # or the sidebar would show a selection that is not there.
            row.set_visible(not collapsed or row._key == self.filter_key)

    def _apply_all_group_states(self):
        for gkey in self.group_rows:
            self._apply_group_state(gkey)

    def _select_category(self, key: str):
        """Go to a category, whether or not its row is already selected.

        GtkListBox emits nothing when you select the row that is already
        selected, so the page never got applied in that case -- which is
        exactly what happens when a restored category and `--page` name the
        same destination, or when another path has already moved the
        selection. Applying it directly makes the call mean what it says.
        """
        row = self.sidebar_rows.get(key)
        if row is None:
            return
        self._collapsed.discard(getattr(row, "_group", ""))
        self._apply_all_group_states()
        if row.is_selected():
            self._on_sidebar_selected(row.get_parent(), row)
        else:
            row.get_parent().select_row(row)

    def _on_sidebar_selected(self, _list, row):
        if row is None:
            return                      # selection cleared during a rebuild
        self.filter_key = row._key
        if getattr(self, "_narrow", False):
            GLib.idle_add(self.split.set_show_sidebar, False)
        if row._key == "featured":
            self._leave_store()
            self._show_page("featured")
            if not self._feat_loaded_once:
                self._feat_loaded_once = True
                self._feat_reload()
            else:
                self._feat_show(self._shelves)
            return

        if row._key == inventory.AGENT:
            self._leave_store()
            self._show_page(inventory.AGENT)
            if not self._ai_loaded_once:
                self._ai_loaded_once = True
                self._ai_reload()
            else:
                self._ai_show(self._ai_survey)
            return

        if row._key == "history":
            self._leave_store()
            self._show_page("history")
            self._history_reload()
            return

        if row._key == "maintenance":
            self._leave_store()
            self._show_page("maintenance")
            if not self._maint_loaded_once:
                self._maint_loaded_once = True
                self._maint_reload()
            else:
                self._maint_show(self._findings)
            return

        if row._key in ("discover", "themes"):
            self._discovering = True
            self._show_page(row._key)
            self.search.grab_focus()
            if not self._store_loaded_once:
                self._store_loaded_once = True
                self._store_reload()
            else:
                self._pin_market(row._key)
                self._store_refresh()
            return
        if self._leave_store():
            self.title.set_title(self._page_title(row._key, "Software"))
            return
        self._show_page("packages", category=row._key)
        self.filter.changed(Gtk.FilterChange.DIFFERENT)
        self._update_status()

    def _set_narrow(self, narrow: bool):
        """Fold or unfold for the window's width."""
        self._narrow = narrow
        # Collapsed, the sidebar is an overlay that should start hidden;
        # expanded, it is furniture and should be back.
        self.split.set_show_sidebar(not narrow)
        self.sidebar_toggle.set_active(not narrow)
        self.sidebar_toggle.set_tooltip_text(
            "Show categories" if narrow else "Hide categories")
        # One column of cards is the right answer on a narrow window; four
        # would each be 110px wide.
        if hasattr(self, "store_grid"):
            self.store_grid.set_max_columns(2 if narrow else 4)

    def _select_category_and_close(self, key: str):
        self._select_category(key)
        if getattr(self, "_narrow", False):
            self.split.set_show_sidebar(False)

    def _page_title(self, key: str, fallback: str) -> str:
        row = self.sidebar_rows.get(key)
        return row.get_title() if row is not None else fallback

    def _show_page(self, key: str, category: str = ""):
        """Apply one page's chrome. The only place that touches it."""
        spec = self.PAGES.get(key) or self.PAGES["packages"]
        self._page = key
        self.content_stack.set_visible_child_name(spec["view"])
        self.title.set_title(
            self._page_title(category, spec["title"]) if category
            else spec["title"])
        drawer = spec.get("filters", False)
        self.sort_btn.set_visible(spec["sort"] or drawer)
        self.sort_btn.set_tooltip_text(
            "Filter and sort the marketplace" if drawer else "Sort by")
        if drawer:
            self.sort_btn.set_menu_model(None)
            self.sort_btn.set_popover(self.store_drawer)
        else:
            self.sort_btn.set_popover(None)
            self._rebuild_sort_menu(self.SORTS_INSTALLED)
        self.search_bar.set_visible(spec["search"] is not None)
        if spec["search"]:
            self.search.set_placeholder_text(spec["search"])
        self.store_back.set_visible(False)
        if spec["view"] == "store":
            self._pin_market(key)
        self._sync_primary()

    def _refresh_page(self):
        """Re-read whatever the current page shows. One action, every page."""
        page = getattr(self, "_page", "packages")
        if page == "featured":
            self._feat_reload()
        elif page == "discover":
            self._store_reload(force=True)
        elif page == "maintenance":
            self._maint_reload()
        elif page == "history":
            self._history_reload()
        elif page == inventory.AGENT:
            self._ai_reload()
        else:
            self.reload()

    def _pin_market(self, page: str):
        """Point the marketplace filter at whatever this page is about.

        The App Store shows every catalogue; Themes is the same view with one
        of them selected. Changing the filter by hand afterwards re-titles the
        page, so the header never claims to be showing something it is not.
        """
        wanted = (self.PAGES.get(page) or {}).get("market", "")
        keys = getattr(self, "_store_markets", [""])
        index = keys.index(wanted) if wanted in keys else 0
        if self.store_market.get_selected() != index:
            self._pinning = True
            self.store_market.set_selected(index)
            self._pinning = False

    def _retitle_store(self):
        """Say what the store is showing when the marketplace filter changes.

        This only edits the title. It used to also select the matching
        sidebar row, which re-entered the selection handler and rebuilt the
        page -- so opening Themes, then letting the catalogue finish loading,
        put you back on the App Store. Which page you are on is the sidebar's
        business and nothing else's.
        """
        if self.content_stack.get_visible_child_name() != "store":
            return
        if getattr(self, "_pinning", False):
            return
        keys = getattr(self, "_store_markets", [""])
        if len(keys) <= 1:
            return
        i = self.store_market.get_selected()
        market = keys[i] if 0 <= i < len(keys) else ""
        page = getattr(self, "_page", "discover")
        if market == "themes":
            self.title.set_title("Themes")
        elif page == "themes" and not market:
            # Un-pinned from the Themes page: it is the whole store now.
            self.title.set_title("App Store")
        else:
            self.title.set_title(self.PAGES.get(page, {}).get("title", "App Store"))

    def _sync_primary(self):
        """Put the current page's main action in the header button."""
        action = (self._primary_actions().get(getattr(self, "_page", "packages")))
        self._primary = action
        if action is None:
            self.update_btn.set_visible(False)
            return
        label, enabled, tooltip = action[0], action[1], action[2]
        self.update_btn.set_visible(True)
        self.update_btn.set_label(label)
        self.update_btn.set_sensitive(enabled)
        self.update_btn.set_tooltip_text(tooltip)

    def _primary_actions(self):
        """(label, enabled, tooltip, handler) for each page that has one."""
        pending = sum(1 for i in range(self.store.get_n_items())
                      if self.store.get_item(i).has_update)
        harnesses = [e for _t, _d, es in getattr(self, "_ai_survey", [])
                     for e in es if e.kind == ai.HARNESS and e.update and e.action]
        free = maintenance.reclaimable_mb(getattr(self, "_findings", []))
        return {
            "packages": (
                f"Update all ({pending})" if pending else "Up to date",
                bool(pending),
                "Runs `omarchy update` in a terminal, which is where it can "
                "ask for your password",
                lambda: self.on_update_all(None)),
            inventory.AGENT: (
                f"Update {len(harnesses)} agent CLI"
                + ("s" if len(harnesses) != 1 else "")
                if harnesses else "Agents current",
                bool(harnesses),
                "Update every agent CLI with a newer release that knows how "
                "to update itself",
                self._ai_update_all),
            "maintenance": (
                f"Reclaim {maintenance.fmt_size(free)}" if free
                else "Nothing to reclaim",
                free > 0,
                "Trim the package cache, clear the AUR build cache and vacuum "
                "the journal — none of it touches anything you created",
                self._reclaim_space),
        }

    def _on_primary(self, _button):
        if self._primary:
            self._primary[3]()

    def _say(self, message: str, timeout: int = 5):
        """A transient result. Replaces four separate dim status labels."""
        toast = Adw.Toast(title=message, timeout=timeout)
        self.toasts.add_toast(toast)

    def _apply_theme(self):
        """Restate the active Omarchy palette as libadwaita's named colours.

        Absence is a valid state: with no Omarchy theme the provider is
        emptied and GTK's own theme shows through unmodified.
        """
        palette = theming.load_palette()
        try:
            self._theme_provider.load_from_data(
                theming.css_for(palette).encode())
        except GLib.Error:
            return
        dark = theming.is_dark(palette)
        if dark is not None:
            Adw.StyleManager.get_default().set_color_scheme(
                Adw.ColorScheme.FORCE_DARK if dark
                else Adw.ColorScheme.FORCE_LIGHT)

    def _on_theme_changed(self):
        # The theme is swapped as several filesystem operations, so this
        # fires a handful of times; coalesce into one restyle.
        if getattr(self, "_theme_timer", None):
            GLib.source_remove(self._theme_timer)
        self._theme_timer = GLib.timeout_add(250, self._theme_tick)

    def _theme_tick(self):
        self._theme_timer = None
        self._apply_theme()
        if getattr(self, "hero", None) is not None:
            self._refresh_hero()
        self._say(theming.describe(), 3)
        return False

    def _busy(self, working: bool):
        """The one place the window says it is doing something."""
        self.busy.set_visible(working)
        if working:
            self.busy.start()
        else:
            self.busy.stop()

    def _summarize(self, text: str, page: str = None):
        """The standing one-line description of what is on screen.

        Scoped to a page because the loaders are asynchronous: the package
        scan finishing half a second after you opened the App Store used to
        overwrite the store's summary with "129 installed · showing 0".
        """
        if page is not None and page != getattr(self, "_page", "packages"):
            return
        self.title.set_subtitle(text)

    def _leave_store(self) -> bool:
        """Put the installed inventory back. True if the store was showing."""
        if not self._discovering:
            return False
        self._discovering = False
        self._show_page("packages")
        self._rebuild_sort_menu(self.SORTS_INSTALLED)
        self._manual_sorter = None
        self._rebuild_sorter()
        self.reload()
        return True

    # ---- the plugin marketplace -------------------------------------------
    def _build_store(self):
        """The App Store page: a card grid over plugins.omarchy.org's catalog.

        A ColumnView was the wrong shape for this. The marketplace's own front
        end shows artwork, a description and a star count per entry, and that
        is what tells two of 5,600 plugins apart -- none of which survives
        being squeezed into table cells.
        """
        page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)

        # Filters live in a drawer behind one header button, not in a bar of
        # eight controls above the results. Every page then has the same
        # chrome in the same places, and the store gets its vertical space
        # back for what it is actually for.
        bar = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8,
                      margin_start=12, margin_end=12,
                      margin_top=8, margin_bottom=12)

        self.store_cat = Gtk.DropDown.new_from_strings(["All categories"])
        self.store_cat.set_tooltip_text("Category")
        self.store_cat.connect("notify::selected", lambda *_: self._store_refresh())
        bar.append(self._labelled("Category", self.store_cat))

        self.store_sort = Gtk.DropDown.new_from_strings(
            [label for _k, label, _f in store_mod.SORTS])
        self.store_sort.set_tooltip_text("Sort by")
        self.store_sort.connect("notify::selected", lambda *_: self._store_refresh())
        bar.append(self._labelled("Sort by", self.store_sort))

        self.store_trust = Gtk.DropDown.new_from_strings(["Any provenance"])
        self.store_trust.set_tooltip_text("Filter by what is known about the source")
        self.store_trust.connect("notify::selected", lambda *_: self._store_refresh())
        bar.append(self._labelled("Provenance", self.store_trust))

        # One marketplace today, so the control would be a dropdown with a
        # single entry. It appears on its own once a second one is registered.
        self.store_market = Gtk.DropDown.new_from_strings(["All marketplaces"])
        self.store_market.set_tooltip_text("Marketplace")
        self.store_market.set_visible(len(store_mod.enabled_markets()) > 1)
        self.store_market.connect(
            "notify::selected",
            lambda *_: (self._retitle_store(), self._store_refresh()))
        bar.append(self._labelled("Marketplace", self.store_market))

        toggles = Gtk.Box(spacing=8, homogeneous=True)
        self.store_verified = Gtk.ToggleButton(label="Verified")
        self.store_verified.set_tooltip_text(
            "Only entries with a review that still applies to the current "
            "upstream commit")
        self.store_verified.connect("toggled", lambda *_: self._store_refresh())
        toggles.append(self.store_verified)

        self.store_installed = Gtk.ToggleButton(label="Installed")
        self.store_installed.set_tooltip_text("Only what is already on this system")
        self.store_installed.connect("toggled", lambda *_: self._store_refresh())
        toggles.append(self.store_installed)
        bar.append(toggles)

        # Packages are not plugins, and mixing 15,000 of them into the
        # marketplace would bury it. The escape hatch stays explicit.
        reset = Gtk.Button(label="Reset filters")
        reset.add_css_class("flat")
        reset.set_tooltip_text("Clear every filter and go back to Most popular")
        reset.connect("clicked", lambda *_: self._store_reset_filters())

        self.store_pkgs = Gtk.Button(label="Search repos and the AUR instead")
        self.store_pkgs.add_css_class("flat")
        self.store_pkgs.set_tooltip_text(
            "Packages are not plugins; 15,000 of them would bury the "
            "marketplace, so they are searched separately")
        self.store_pkgs.connect("clicked", lambda *_: self._store_packages())

        bar.append(reset)
        bar.append(self.store_pkgs)

        self.store_drawer = Gtk.Popover()
        self.store_drawer.set_child(bar)
        self.filter_btn.set_popover(self.store_drawer)

        self.store_model = Gio.ListStore(item_type=PluginItem)
        sel = Gtk.SingleSelection(model=self.store_model)
        sel.set_can_unselect(True)
        sel.set_autoselect(False)

        factory = Gtk.SignalListItemFactory()
        factory.connect("setup", self._card_setup)
        factory.connect("bind", self._card_bind)
        factory.connect("unbind", self._card_unbind)

        self.store_grid = Gtk.GridView(model=sel, factory=factory)
        self.store_grid.set_max_columns(4)
        self.store_grid.set_min_columns(1)
        self.store_grid.set_single_click_activate(True)
        self.store_grid.connect("activate", self._on_card_activated)
        self.store_grid.add_css_class("navigation-sidebar")

        scroller = Gtk.ScrolledWindow(vexpand=True)
        scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroller.set_child(self.store_grid)
        scroller.set_margin_start(12)
        scroller.set_margin_end(12)
        scroller.set_margin_bottom(12)
        self.store_scroll_inner = scroller
        # Cards arrive a page at a time as you reach the end of the list.
        # GridView recycles the widgets, but building 5,600 PluginItems on
        # every keystroke is what makes the search feel heavy.
        scroller.get_vadjustment().connect("value-changed", self._store_scrolled)
        self.store_scroller = scroller

        # The grid and its empty state occupy the same slot.
        self.store_stack = Gtk.Stack()
        self.store_stack.set_vexpand(True)
        self.store_stack.add_named(scroller, "grid")
        retry = Gtk.Button(label="Try again", halign=Gtk.Align.CENTER)
        retry.add_css_class("suggested-action")
        retry.add_css_class("pill")
        retry.connect("clicked", lambda *_: self._store_reload(force=True))
        self.store_empty = self._status_page(
            "system-search-symbolic", "Nothing matches",
            "Try a different search, or clear the filters.", None)
        self.store_offline = self._status_page(
            "network-offline-symbolic", "The catalogue could not be loaded",
            "Almanac could not reach the marketplaces. It will use the last "
            "copy it has as soon as one exists.", retry)
        self.store_stack.add_named(self.store_empty, "empty")
        self.store_stack.add_named(self.store_offline, "offline")
        page.append(self.store_stack)
        return page

    STORE_PAGE = 100

    def _store_scrolled(self, adj):
        # Within two viewports of the end: start the next page before the user
        # arrives at the gap, so scrolling never stops on an empty row.
        if adj.get_value() + adj.get_page_size() * 3 >= adj.get_upper():
            self._store_extend()

    def _store_extend(self):
        """Append the next page of results, if there are any left."""
        have = self.store_model.get_n_items()
        results = getattr(self, "_store_results", [])
        if have >= len(results):
            return False
        for p in results[have:have + self.STORE_PAGE]:
            self.store_model.append(PluginItem(p))
        self._store_count()
        # A short result set may not fill the viewport, in which case no
        # scroll event will ever arrive to ask for the rest.
        GLib.idle_add(self._store_fill)
        return True

    def _store_fill(self):
        adj = self.store_scroller.get_vadjustment()
        if adj.get_upper() <= adj.get_page_size() + 1:
            self._store_extend()
        return False

    def _store_count(self):
        """The store's standing summary, worded for whichever page is showing.

        Themes pin the marketplace filter, so "a filter is set" is always true
        there and cannot be what decides the wording -- the page has to.
        """
        page = getattr(self, "_page", "discover")
        shown = self.store_model.get_n_items()
        results = getattr(self, "_store_results", [])
        noun = "themes" if page == "themes" else "plugins"

        # A pinned marketplace is the page, not a filter the user applied.
        pinned = bool((self.PAGES.get(page) or {}).get("market"))
        narrowed = (self.search.get_text().strip()
                    or getattr(self, "_store_cat_key", "")
                    or self.store_verified.get_active()
                    or self.store_installed.get_active()
                    or self.store_trust.get_selected() > 0
                    or (self.store_market.get_selected() > 0 and not pinned))

        pool = ([p for p in getattr(self, "_plugins", [])
                 if p.install_kind == "theme"] if page == "themes"
                else getattr(self, "_plugins", []))
        total = len(pool)
        installed = sum(1 for p in pool if p.installed)

        if shown < len(results):
            self._summarize(f"{shown} of {len(results)} shown", page)
        elif narrowed:
            self._summarize(f"{len(results)} of {total} {noun}", page)
        elif page == "themes":
            self._summarize(
                f"{total} Omarchy themes · {installed} installed", page)
        else:
            markets = ", ".join(m.name for m in store_mod.enabled_markets())
            self._summarize(
                f"{total} listings from {markets} · {installed} installed here",
                page)

    # ---- card widgets ----------------------------------------------------
    def _card_setup(self, _factory, item):
        card = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        card.add_css_class("store-card")
        card.set_size_request(224, -1)
        card.set_overflow(Gtk.Overflow.HIDDEN)

        # Artwork: a screenshot when the entry has one, the accent tile when
        # it does not. Both occupy the same box so the grid stays aligned.
        shot = Gtk.Box()
        shot.add_css_class("store-shot")
        shot.set_size_request(-1, 142)
        shot.set_overflow(Gtk.Overflow.HIDDEN)
        pic = Gtk.Picture()
        pic.set_content_fit(Gtk.ContentFit.COVER)
        pic.set_hexpand(True)
        pic.set_vexpand(True)
        # The art must not get a vote on how wide the card is. A GtkPicture
        # reports its paintable's size as its natural width, and GridView
        # chooses columns from natural width, so a 16:9 screenshot in a card
        # meant it laid out one column however narrow the card was told to be.
        # A non-propagating ScrolledWindow is the standard way to cut that.
        art_clamp = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
        art_clamp.set_policy(Gtk.PolicyType.EXTERNAL, Gtk.PolicyType.EXTERNAL)
        art_clamp.set_propagate_natural_width(False)
        art_clamp.set_propagate_natural_height(False)
        art_clamp.set_child(pic)
        initials = Gtk.Label()
        initials.add_css_class("store-initials")
        initials.set_hexpand(True)
        initials.set_vexpand(True)
        shot.append(art_clamp)
        shot.append(initials)
        card.append(shot)

        body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2,
                       margin_start=12, margin_end=12,
                       margin_top=8, margin_bottom=8)

        top = Gtk.Box(spacing=4)
        name = Gtk.Label(xalign=0, ellipsize=Pango.EllipsizeMode.END)
        name.set_max_width_chars(22)
        name.add_css_class("store-name")
        name.set_hexpand(True)
        check = Gtk.Image(icon_name="emblem-ok-symbolic", pixel_size=12)
        check.add_css_class("store-verified")
        check.set_tooltip_text("Source reviewed by the marketplace")
        spacer = Gtk.Box(hexpand=True)
        installed = Gtk.Label(label="Installed",
                              ellipsize=Pango.EllipsizeMode.END)
        installed.set_max_width_chars(10)
        installed.add_css_class("pill-ok")
        top.append(name)
        top.append(check)
        top.append(spacer)
        top.append(installed)
        body.append(top)

        sub = Gtk.Label(xalign=0, ellipsize=Pango.EllipsizeMode.END)
        sub.set_max_width_chars(24)
        sub.add_css_class("caption")
        sub.add_css_class("dim-label")
        body.append(sub)

        # Provenance, on every card: the repository the code actually comes
        # from. The author line above it is self-declared in the plugin's own
        # manifest and proves nothing by itself.
        prov = Gtk.Box(spacing=4)
        prov_icon = Gtk.Image(icon_name="folder-remote-symbolic", pixel_size=11)
        prov_icon.add_css_class("dim-label")
        prov_text = Gtk.Label(xalign=0, ellipsize=Pango.EllipsizeMode.MIDDLE)
        prov_text.set_max_width_chars(26)
        prov_text.add_css_class("caption")
        prov_text.add_css_class("dim-label")
        prov_text.add_css_class("store-prov")
        prov.append(prov_icon)
        prov.append(prov_text)
        body.append(prov)

        desc = Gtk.Label(xalign=0, wrap=True, lines=2,
                         ellipsize=Pango.EllipsizeMode.END)
        desc.set_max_width_chars(30)
        desc.set_width_chars(20)
        desc.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        desc.add_css_class("caption")
        desc.add_css_class("dim-label")
        desc.set_valign(Gtk.Align.START)
        desc.set_vexpand(True)
        body.append(desc)

        bottom = Gtk.Box(spacing=6, margin_top=4)
        cat = Gtk.Label(ellipsize=Pango.EllipsizeMode.END)
        cat.set_max_width_chars(11)
        cat.add_css_class("pill")
        trust = Gtk.Label(ellipsize=Pango.EllipsizeMode.END)
        trust.set_max_width_chars(14)
        stars = Gtk.Label(ellipsize=Pango.EllipsizeMode.END)
        stars.set_max_width_chars(8)
        stars.add_css_class("store-stars")
        stars.add_css_class("dim-label")
        bottom.append(cat)
        bottom.append(trust)
        bottom.append(stars)
        bottom.append(Gtk.Box(hexpand=True))
        action = Gtk.Button()
        action.set_can_shrink(True)
        bottom.append(action)
        body.append(bottom)

        card.append(body)
        item.set_child(card)
        # Stash the leaves: bind runs on every scroll step and walking the
        # widget tree each time would be the expensive part.
        item._parts = dict(pic=pic, art=art_clamp, initials=initials,
                           shot=shot, name=name,
                           check=check, installed=installed, sub=sub,
                           desc=desc, cat=cat, stars=stars, action=action,
                           trust=trust, prov=prov_text, prov_row=prov)
        item._handler = None
        item._token = 0

    def _card_bind(self, _factory, item):
        p = item.get_item().plugin
        w = item._parts
        item._token += 1
        token = item._token

        w["name"].set_text(p.name)
        w["name"].set_tooltip_text(p.name)
        w["check"].set_visible(p.verified)
        w["installed"].set_visible(p.installed)
        w["sub"].set_text(p.subtitle)
        w["desc"].set_text(p.description or p.install_note or "")
        w["cat"].set_text(p.category)
        w["stars"].set_text(f"★ {p.stars}" if p.stars else "")

        # Provenance
        t = p.trust
        w["prov"].set_text(p.publisher)
        w["prov_row"].set_tooltip_text(
            f"{p.publisher}\nListed by {p.market_name}" if p.repo
            else f"Listed by {p.market_name}")
        w["trust"].set_text(t.label)
        for css in ("pill-accent", "pill-ok", "pill-warn", "pill-quiet"):
            w["trust"].remove_css_class(css)
        w["trust"].add_css_class(t.css)
        w["trust"].set_tooltip_text(t.why)
        # The tick duplicates the chip; keep it only where it adds the
        # marketplace's own verified flag on top of a human review.
        w["check"].set_visible(p.verified and t.key == "reviewed")

        # Artwork
        for name in store_mod.ACCENTS:
            w["shot"].remove_css_class(f"acc-{name}")
        w["shot"].remove_css_class("acc-none")
        cached = store_mod.thumb_path(p)
        if cached is not None and cached.exists() and cached.stat().st_size:
            self._card_art(item, token, cached)
        elif cached is not None:
            w["art"].set_visible(False)
            w["initials"].set_visible(True)
            w["initials"].set_text(p.initials or p.name[:2].upper())
            w["shot"].add_css_class(f"acc-{p.accent}" if p.accent in store_mod.ACCENTS
                                    else "acc-none")
            self._thumb_async(item, token, p)
        else:
            w["art"].set_visible(False)
            w["initials"].set_visible(True)
            w["initials"].set_text(p.initials or p.name[:2].upper())
            w["shot"].add_css_class(f"acc-{p.accent}" if p.accent in store_mod.ACCENTS
                                    else "acc-none")

        # Primary action
        btn = w["action"]
        if item._handler is not None:
            btn.disconnect(item._handler)
            item._handler = None
        # `.flat` strips the accent background but not the accent foreground,
        # so combining it with `.suggested-action` paints accent_fg_color --
        # near-black under most dark themes -- straight onto the card. That
        # measured 1.41:1 against the card. Filled, the same pair is 8.65:1.
        for css in ("suggested-action", "destructive-action", "flat"):
            btn.remove_css_class(css)
        if p.installed and p.install_kind == "theme":
            # An installed theme's useful action is wearing it; removing one
            # is rare enough to belong in the detail sheet, where Remove sits
            # for every other kind too.
            btn.set_label("Apply")
            btn.set_sensitive(True)
            btn.add_css_class("suggested-action")      # filled: the main action
            btn.set_tooltip_text(f"Switch the whole desktop to {p.name}")
            item._handler = btn.connect(
                "clicked", lambda _b, pl=p: self._apply_theme_now(pl))
        elif p.installed:
            btn.set_label("Remove")
            btn.set_sensitive(not p.built_in)
            # Low emphasis: removal should never be the loudest thing on a card.
            btn.add_css_class("flat")
            btn.set_tooltip_text(
                "Ships with Omarchy — cannot be removed" if p.built_in
                else f"Remove {p.name}")
            item._handler = btn.connect("clicked", self.on_plugin_remove, p)
        elif p.installable and p.install_cmd:
            # A glyph rather than a word: cards are narrow, and the word is
            # still there for the pointer (tooltip) and screen readers.
            btn.set_icon_name(INSTALL_ICON)
            btn.set_sensitive(True)
            btn.add_css_class("suggested-action")      # filled: the main action
            btn.set_tooltip_text(f"Install {p.name}\n{p.install_cmd}")
            btn.update_property([Gtk.AccessibleProperty.LABEL],
                                [f"Install {p.name}"])
            item._handler = btn.connect("clicked", self.on_plugin_install, p)
        else:
            # 1,062 entries are listed but not installable through the CLI --
            # suites with their own installers, mostly. Saying so beats an
            # Install button that cannot work.
            # These are listed but have no one-command install -- suites with
            # their own installers, mostly. "Manual setup" is what the
            # marketplace calls it, and it beats an Install that cannot work.
            btn.set_label("Manual setup")
            btn.set_sensitive(True)
            btn.add_css_class("flat")                  # not an install: quieter
            btn.set_tooltip_text(p.install_note or "Not installable from here")
            item._handler = btn.connect(
                "clicked", lambda _b, pl=p: self._show_plugin(pl))

    def _card_unbind(self, _factory, item):
        item._token += 1                # cancels any in-flight image load
        if item._handler is not None:
            item._parts["action"].disconnect(item._handler)
            item._handler = None

    # Scaled for memory, not for layout: 300 cached cards at source
    # resolution would be well over a gigabyte of textures.
    CARD_ART_WIDTH = 420

    def _texture(self, path, max_width=0):
        """Decode once, and not at the source resolution.

        GtkPicture reports its paintable's intrinsic size as its natural size,
        and GridView picks its column count from natural width. The marketplace
        ships 1600px-wide screenshots, so every card asked for 1600px and
        exactly one column fitted however narrow the cards were told to be.
        Scaling at decode time bounds the card and keeps 5,600 of them out of
        memory at full size.
        """
        key = f"{path}@{max_width}"
        tex = self._tex_cache.get(key)
        if tex is not None:
            return tex
        try:
            if max_width:
                pb = GdkPixbuf.Pixbuf.new_from_file_at_scale(
                    str(path), max_width, -1, True)
                tex = Gdk.MemoryTexture.new(
                    pb.get_width(), pb.get_height(),
                    Gdk.MemoryFormat.R8G8B8A8 if pb.get_has_alpha()
                    else Gdk.MemoryFormat.R8G8B8,
                    GLib.Bytes.new(pb.get_pixels()), pb.get_rowstride())
            else:
                tex = Gdk.Texture.new_from_filename(str(path))
        except (GLib.Error, OSError):
            return None
        # A soft cap: scrolling the whole catalogue would otherwise hold every
        # decoded screenshot at once.
        if len(self._tex_cache) > 160:
            self._tex_cache.clear()
        self._tex_cache[key] = tex
        return tex

    def _card_art(self, item, token, path):
        """Put a cached image on a card, decoding at most once per file."""
        if item._token != token:
            return
        tex = self._texture(path, self.CARD_ART_WIDTH)
        if tex is None:
            return
        item._parts["initials"].set_visible(False)
        item._parts["pic"].set_paintable(tex)
        item._parts["art"].set_visible(True)

    def _thumb_async(self, item, token, plugin):
        def done(path):
            if path:
                self._card_art(item, token, path)
            return False

        def work():
            path = store_mod.fetch_thumb(plugin)
            GLib.idle_add(done, path)

        self._thumb_pool.submit(work)

    # ---- marketplace data ------------------------------------------------
    def _store_reload(self, force: bool = False):
        self._busy(True)
        self._summarize("Loading the plugin marketplace…", "discover")

        def work():
            try:
                plugins = store_mod.catalog(force=force)
                store_mod.annotate(plugins)
            except Exception as exc:                       # noqa: BLE001
                GLib.idle_add(self._store_failed, str(exc))
                return
            GLib.idle_add(self._store_loaded, plugins)

        threading.Thread(target=work, daemon=True).start()

    def _store_failed(self, message):
        self._busy(False)
        self._say(f"Could not load the catalog: {message}")
        return False

    def _store_loaded(self, plugins):
        self._busy(False)
        self._plugins = plugins
        names = ["All categories"] + [
            f"{c} ({n})" for c, n in store_mod.categories(plugins)]
        self._store_cats = [""] + [c for c, _n in store_mod.categories(plugins)]
        model = Gtk.StringList.new(names)
        # Setting the model resets the selection, which would re-enter the
        # refresh; restore the chosen category afterwards instead.
        want = getattr(self, "_store_cat_key", "")
        self.store_cat.set_model(model)
        if want in self._store_cats:
            self.store_cat.set_selected(self._store_cats.index(want))

        trusts = store_mod.trust_counts(plugins)
        self._store_trusts = [""] + [k for k, _l, _n in trusts]
        self.store_trust.set_model(Gtk.StringList.new(
            ["Any provenance"] + [f"{lbl} ({n})" for _k, lbl, n in trusts]))

        markets = store_mod.market_counts(plugins)
        self._pinning = True
        self._store_markets = [""] + [mid for mid, _nm, _n in markets]
        self.store_market.set_model(Gtk.StringList.new(
            ["All marketplaces"] + [f"{nm} ({n})" for _mid, nm, n in markets]))
        self.store_market.set_visible(len(markets) > 1)
        self._pinning = False

        self._pin_market(getattr(self, "_page", "discover"))
        self._store_refresh()
        return False

    def _store_refresh(self):
        if not getattr(self, "_plugins", None):
            return
        idx = self.store_cat.get_selected()
        cats = getattr(self, "_store_cats", [""])
        category = cats[idx] if 0 <= idx < len(cats) else ""
        self._store_cat_key = category
        sort = store_mod.SORTS[self.store_sort.get_selected()][0] \
            if self.store_sort.get_selected() < len(store_mod.SORTS) else "popular"

        def pick(widget, keys):
            i = widget.get_selected()
            return keys[i] if 0 <= i < len(keys) else ""

        results = store_mod.browse(
            self._plugins, query=self.search.get_text(), category=category,
            sort=sort, installed_only=self.store_installed.get_active(),
            verified_only=self.store_verified.get_active(),
            trust=pick(self.store_trust, getattr(self, "_store_trusts", [""])),
            market=pick(self.store_market, getattr(self, "_store_markets", [""])))

        self._store_results = results
        if not self._plugins:
            self.store_stack.set_visible_child_name("offline")
        elif not results:
            clearable = (self.search.get_text().strip()
                         or getattr(self, "_store_cat_key", "")
                         or self.store_verified.get_active()
                         or self.store_installed.get_active()
                         or self.store_trust.get_selected() > 0)
            self.store_empty.set_description(
                "Try a different search, or clear the filters." if clearable
                else "This marketplace has nothing listed right now.")
            self.store_stack.set_visible_child_name("empty")
        else:
            self.store_stack.set_visible_child_name("grid")
        self.store_model.remove_all()
        # Back to the top: the previous scroll position means nothing once
        # the result set has changed underneath it.
        self.store_scroller.get_vadjustment().set_value(0)
        self._store_extend()
        return False

    def _store_queue(self):
        """Debounce the search box: re-sorting 5,600 entries per keystroke."""
        if self._store_timer:
            GLib.source_remove(self._store_timer)
        self._store_timer = GLib.timeout_add(180, self._store_tick)

    def _store_tick(self):
        self._store_timer = None
        self._store_refresh()
        return False

    def _store_packages(self):
        """Hand the current query to the repo/AUR search, which is a list."""
        query = self.search.get_text().strip()
        if len(query) < 2:
            self._say(
                "Type at least two characters to search repos and the AUR")
            return
        self.content_stack.set_visible_child_name("list")
        self.title.set_title("Repositories and AUR")
        self.update_btn.set_visible(False)
        self.store_back.set_visible(True)
        self._rebuild_sort_menu(self.SORTS_DISCOVER)
        self.sort_btn.set_visible(True)
        self._manual_sorter = None
        self._rebuild_sorter()
        self._run_discover()

    def _store_return(self):
        self._show_page("discover")
        self._store_refresh()

    # ---- marketplace actions ---------------------------------------------
    def _on_card_activated(self, _grid, position):
        item = self.store_model.get_item(position)
        if item:
            self._show_plugin(item.plugin)

    def on_plugin_install(self, _button, p):
        """Install from the store, without leaving the app."""
        if p.install_kind == "appimage":
            self._install_store_appimage(p)
            return
        if p.install_kind == "theme":
            self._install_theme(p)
            return

        argv = shlex.split(p.install_cmd)
        # `plugin add` has a prompt to skip; `plugin enable` does not take the
        # flag at all, so appending it blindly would break the built-ins.
        if "add" in argv[:3] and "--yes" not in argv:
            argv.append("--yes")

        detail = (f"Publisher   {p.publisher}\n"
                  f"Provenance  {p.trust.label} — {p.trust.why}\n"
                  f"Licence     {p.license or 'unstated'}\n"
                  f"Listed by   {p.market_name}\n\n"
                  f"$ {' '.join(argv)}")
        tasks.confirm(
            self, f"Install {p.name}?",
            "Omarchy shell plugins run inside the shell process with your "
            "permissions — there is no sandbox. Install what you trust.",
            detail, "Install", False,
            lambda: tasks.run(self, f"Installing {p.name}",
                              [(f"Installing {p.name}", argv)],
                              lambda ok: self._store_after(ok, p, "installed")))

    def on_plugin_remove(self, _button, p):
        """Remove, with the command that kind of entry actually uses.

        Three catalogues, three removal mechanisms. Dispatching on the kind
        rather than assuming `omarchy plugin remove` matters: run against a
        theme that command addresses something that does not exist.
        """
        if p.install_kind == "appimage":
            self._remove_store_appimage(p)
            return
        if p.install_kind == "theme":
            self._remove_theme(p)
            return
        tasks.confirm(
            self, f"Remove {p.name}?",
            f"The plugin and its cloned repository are deleted from "
            f"~/.config/omarchy/plugins.",
            f"$ omarchy plugin remove {p.id} --yes",
            "Remove", True,
            lambda: tasks.run(self, f"Removing {p.name}",
                              [(f"Removing {p.name}",
                                ["omarchy", "plugin", "remove", p.id, "--yes"])],
                              lambda ok: self._store_after(ok, p, "removed")))

    # ---- store entries that are themes ----
    def _install_theme(self, p):
        """Themes install and apply as two steps, so they are offered as two.

        `omarchy theme install` clones it; `omarchy theme set` switches the
        whole desktop over. Doing both on one click would re-skin everything
        the moment someone clicked Install out of curiosity.
        """
        if p.installed:
            self._apply_theme_now(p)
            return
        url = p.install_cmd.split()[-1]
        tasks.confirm(
            self, f"Install the {p.name} theme?",
            "The theme is cloned into ~/.config/omarchy/themes. Nothing "
            "changes appearance until you apply it.",
            f"Publisher  {p.publisher}\n"
            f"Licence    {p.license or 'unstated'}\n"
            f"Stars      {p.stars}\n\n"
            f"$ omarchy theme install {url}",
            "Install", False,
            lambda: tasks.run(
                self, f"Installing {p.name}",
                [(f"Installing {p.name}",
                  ["omarchy", "theme", "install", url])],
                lambda ok: self._theme_installed(ok, p)))

    def _theme_installed(self, ok, p):
        if not ok:
            self._say(f"{p.name} was not installed", 8)
            return False
        store_mod.annotate(self._plugins)
        self._store_refresh()
        toast = Adw.Toast(title=f"{p.name} installed", timeout=0)
        toast.set_button_label("Apply")
        toast.connect("button-clicked", lambda *_: self._apply_theme_now(p))
        self.toasts.add_toast(toast)
        return False

    def _apply_theme_now(self, p):
        tasks.confirm(
            self, f"Switch to {p.name}?",
            "Applies the theme to the whole desktop — bar, terminal, editor "
            "and this window. Your previous theme stays installed.",
            f"$ omarchy theme set {p.name}", "Apply", False,
            lambda: tasks.run(self, f"Applying {p.name}",
                              [(f"Applying {p.name}",
                                ["omarchy", "theme", "set", p.name])],
                              lambda ok: self._after_theme(ok, p)))

    def _after_theme(self, ok, p):
        if ok:
            self._apply_theme()
            if getattr(self, "hero", None) is not None:
                self._refresh_hero()
            self._say(f"Now wearing {p.name}")
        return False

    def _remove_theme(self, p):
        if p.built_in:
            self._say(f"{p.name} ships with Omarchy and cannot be removed", 6)
            return
        tasks.confirm(
            self, f"Remove the {p.name} theme?",
            "Deletes the cloned theme from ~/.config/omarchy/themes. If it is "
            "the theme you are wearing, Omarchy falls back to another one.",
            f"$ omarchy theme remove {p.name}", "Remove", True,
            lambda: tasks.run(self, f"Removing {p.name}",
                              [(f"Removing {p.name}",
                                ["omarchy", "theme", "remove", p.name])],
                              lambda ok: self._store_after(ok, p, "removed")))

    # ---- store entries that are AppImages, not plugins ----
    def _install_store_appimage(self, p):
        dest = Path(inventory.REGISTRY).parent / "apps" / f"{p.id}.AppImage"
        detail = (f"Publisher   {p.publisher}\n"
                  f"Provenance  {p.trust.label} — {p.trust.why}\n"
                  f"Licence     {p.license or 'unstated'}\n"
                  f"Version     {p.version}\n"
                  f"Download    {p.asset_name} ({p.size_mb:.0f} MB)\n"
                  f"Installs to {dest}")
        tasks.confirm(
            self, f"Install {p.name}?",
            f"{p.size_mb:.0f} MB will be downloaded from GitHub and kept "
            f"with Almanac's apps, with a launcher entry.",
            detail, "Install", False,
            lambda: tasks.run(
                self, f"Installing {p.name}",
                [(f"Downloading {p.name} {p.version}",
                  lambda log, p=p, dest=dest: self._fetch_appimage(log, p, dest))],
                lambda ok, holder=self._watch_defaults(): (
                    ok and self._check_defaults(holder),
                    self._store_after(ok, p, "installed"))))

    def _fetch_appimage(self, log, p, dest):
        """Download, register and add a launcher entry. Raises on failure."""
        dest.parent.mkdir(parents=True, exist_ok=True)
        log(f"{p.asset_name}  ({p.size_mb:.0f} MB)")
        log(f"from {p.download_url}")

        seen = [0]

        def progress(frac):
            pct = int(frac * 100)
            if pct >= seen[0] + 10:         # one line per 10%, not per chunk
                seen[0] = pct
                log(f"  {pct}%")

        appimage_mod.download(p.download_url, dest, progress)
        log("downloaded, registering")

        reg = inventory._load_registry()
        reg[p.id] = {
            "name": p.id, "path": str(dest), "version": p.version,
            "description": p.description, "url": p.repo,
            "latest": {}, "checked_at": 0,
            # Tracked the same way a hand-added AppImage is, so the existing
            # update check picks it up without knowing about marketplaces.
            "update_spec": {"kind": "github", "owner": p.owner,
                            "repo": p.id, "release": "latest",
                            "pattern": f"{p.id}-*-linux-x86_64.AppImage"},
        }
        inventory.save_registry(reg)
        if appimage_mod.install_desktop_entry(dest, p.id):
            log("launcher entry added")
        log(f"{p.name} {p.version} installed")

    def _remove_store_appimage(self, p):
        entry = inventory.App(name=p.name, version=p.local_version or p.version,
                              source=inventory.APPIMAGE,
                              exec_path=str(Path(inventory.REGISTRY).parent
                                            / "apps" / f"{p.id}.AppImage"),
                              extra={"key": p.id})
        tasks.confirm(
            self, f"Remove {p.name}?",
            "The AppImage, its rollback copy and its launcher entry are "
            "deleted. Documents you created with it are untouched.",
            f"{entry.exec_path}\n{entry.exec_path.replace('.AppImage', '.previous')}",
            "Remove", True,
            lambda: (self._remove_appimage(entry),
                     self._store_after(True, p, "removed")))

    def _store_after(self, ok, p, verb):
        """The app ran the command, so it knows when to re-read the system."""
        if not ok:
            self._say(f"{p.name} was not {verb}")
            return
        store_mod.annotate(self._plugins)
        self._store_refresh()
        self._say(f"{p.name} {verb}")
        if p.install_kind == "appimage":
            self.reload()

    def _show_plugin(self, p):
        """The full listing for one plugin, as the marketplace shows it."""
        dialog = Adw.Dialog(title=p.name)
        dialog.set_content_width(620)
        dialog.set_content_height(680)

        view = Adw.ToolbarView()
        view.add_top_bar(Adw.HeaderBar())

        page = Adw.PreferencesPage()

        hero = Adw.PreferencesGroup()
        head = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)

        shot = Gtk.Box()
        shot.add_css_class("store-shot")
        shot.set_size_request(-1, 300)
        shot.set_overflow(Gtk.Overflow.HIDDEN)
        pic = Gtk.Picture()
        pic.set_content_fit(Gtk.ContentFit.COVER)
        pic.set_hexpand(True)
        pic.set_vexpand(True)
        if p.preview:
            shot.append(pic)
            head.append(shot)

            def load():
                path = store_mod.fetch_thumb(p, detail=True)

                def show():
                    if path:
                        tex = self._texture(path, 640)
                        if tex is not None:
                            pic.set_paintable(tex)
                    return False
                GLib.idle_add(show)
            self._thumb_pool.submit(load)

        title = Gtk.Label(label=p.name, xalign=0, wrap=True)
        title.add_css_class("title-2")
        head.append(title)
        if p.subtitle:
            byline = Gtk.Label(label=p.subtitle, xalign=0)
            byline.add_css_class("dim-label")
            head.append(byline)

        badges = Gtk.Box(spacing=6)
        badges.set_halign(Gtk.Align.START)
        for text, css in self._plugin_badges(p):
            lbl = Gtk.Label(label=text)
            lbl.add_css_class(css)
            badges.append(lbl)
        head.append(badges)

        if p.description:
            body = Gtk.Label(label=p.description, xalign=0, wrap=True)
            body.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
            head.append(body)
        if p.install_note:
            note = Gtk.Label(label=p.install_note, xalign=0, wrap=True)
            note.add_css_class("dim-label")
            note.add_css_class("caption")
            head.append(note)

        hero.add(head)
        page.add(hero)

        t = p.trust
        prov = Adw.PreferencesGroup(
            title="Provenance",
            description="Where this comes from and what has actually been "
                        "checked about it")
        self._detail_row(prov, "Marketplace", p.market_name)
        self._detail_row(prov, "Publisher", p.publisher)
        self._detail_row(prov, "Declared author", p.author)
        self._detail_row(prov, "Assurance", f"{t.label} — {t.why}")
        if p.review_method:
            self._detail_row(prov, "Review", p.review_method.replace("-", " "))
        self._detail_row(prov, "Reviewed by", p.reviewed_by)
        self._detail_row(prov, "Validated commit",
                         f"{p.validated_commit[:12]} on {p.validated_branch}"
                         if p.validated_commit else "")
        self._detail_row(prov, "Validated", p.validated_at[:10])
        self._detail_row(prov, "Upstream commit now",
                         f"{p.upstream_commit[:12]}"
                         + ("  (differs from what was validated)"
                            if p.drifted else "  (matches)")
                         if p.upstream_commit else "")
        self._detail_row(prov, "Upstream last checked",
                         f"{p.upstream_checked_at[:10]}"
                         + (f" — {p.upstream_status}" if p.upstream_status else "")
                         if p.upstream_checked_at else "")
        self._detail_row(prov, "Manifest", p.manifest_path)
        page.add(prov)

        facts = Adw.PreferencesGroup(title="Listing")
        for label, value in (
                ("Identifier", p.id),
                ("Version", p.version),
                ("Category", p.category),
                ("Kind", p.kind),
                ("Status", p.status),
                ("Tags", ", ".join(p.tags)),
                ("Stars", str(p.stars) if p.stars else ""),
                ("Licence", p.license),
                ("Updated", p.updated[:10]),
                ("Listed", p.added[:10]),
                ("Install command", p.install_cmd)):
            self._detail_row(facts, label, value)
        page.add(facts)

        links = Adw.PreferencesGroup()
        for label, url, icon in (
                ("Marketplace listing", p.page, "web-browser-symbolic"),
                ("Source repository", p.repo, "code-symbolic")):
            if not url:
                continue
            row = self._plain(title=label, subtitle=url, activatable=True)
            row.add_suffix(Gtk.Image(icon_name=icon))
            row.connect("activated", lambda _r, u=url: self.open_url(u))
            links.add(row)
        page.add(links)

        view.set_content(page)

        bottom = Gtk.Box(spacing=8, margin_start=12, margin_end=12,
                         margin_top=8, margin_bottom=12,
                         halign=Gtk.Align.END)
        if p.installed and p.install_kind == "theme":
            apply = Gtk.Button(label="Apply")
            apply.add_css_class("suggested-action")
            apply.connect("clicked", lambda b: (dialog.close(),
                                                self._apply_theme_now(p)))
            if not p.built_in:
                rm = Gtk.Button(label="Remove")
                rm.add_css_class("destructive-action")
                rm.connect("clicked", lambda b: (dialog.close(),
                                                 self.on_plugin_remove(b, p)))
                bottom.append(rm)
            bottom.append(apply)
        elif p.installed and not p.built_in:
            rm = Gtk.Button(label="Remove")
            rm.add_css_class("destructive-action")
            rm.connect("clicked", lambda b: (dialog.close(),
                                             self.on_plugin_remove(b, p)))
            bottom.append(rm)
        elif p.installed and p.built_in:
            lbl = Gtk.Label(label="Built in")
            lbl.add_css_class("pill-ok")
            bottom.append(lbl)
        elif p.installable and p.install_cmd:
            ins = Gtk.Button(child=Adw.ButtonContent(icon_name=INSTALL_ICON,
                                                     label="Install"))
            ins.add_css_class("suggested-action")
            ins.connect("clicked", lambda b: (dialog.close(),
                                              self.on_plugin_install(b, p)))
            bottom.append(ins)
        view.add_bottom_bar(bottom)

        dialog.set_child(view)
        dialog.present(self)

    @staticmethod
    def _plugin_badges(p):
        out = []
        if p.installed:
            out.append(("Installed" if not p.built_in else "Built in",
                        "pill-ok"))
        out.append((p.trust.label, p.trust.css))
        if p.status and p.status != "Available":
            out.append((p.status, "pill"))
        if p.stars:
            out.append((f"★ {p.stars}", "pill"))
        if p.category:
            out.append((p.category, "pill"))
        return out

    # ---- maintenance ------------------------------------------------------
    def _build_maintenance(self):
        """One page per question the system can answer about its own health."""
        page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)

        body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=20,
                       margin_start=12, margin_end=12, margin_bottom=16)
        section = self._section(
            "System health",
            "Each check reads the system and offers the one command that acts "
            "on what it found. Nothing here runs until you ask it to.")
        self.maint_list = section._rows
        body.append(section)

        scroller = Gtk.ScrolledWindow(vexpand=True)
        scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroller.set_child(body)
        page.append(scroller)
        return page

    # ---- history ----------------------------------------------------------
    _HISTORY_ICONS = {
        history.PACKAGES: "package-x-generic-symbolic",
        history.SNAPSHOT: "camera-photo-symbolic",
        history.RESET: "edit-undo-symbolic",
        history.ALMANAC: "emblem-ok-symbolic",
    }

    def _build_history(self):
        """What changed on this machine, by day. See history.py."""
        page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self.history_body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL,
                                    spacing=20, margin_start=12, margin_end=12,
                                    margin_bottom=16)
        scroller = Gtk.ScrolledWindow(vexpand=True)
        scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroller.set_child(self.history_body)
        page.append(scroller)
        return page

    def _history_reload(self):
        self._summarize("Reading the last 30 days…", "history")

        def work():
            days = history.timeline()
            GLib.idle_add(self._history_show, days)
        threading.Thread(target=work, daemon=True).start()

    def _history_show(self, days):
        while (child := self.history_body.get_first_child()) is not None:
            self.history_body.remove(child)
        if not days:
            self.history_body.append(self._status_page(
                "document-open-recent-symbolic", "Nothing recorded yet",
                "Package changes, snapshots and settings resets from the last "
                "30 days appear here."))
            self._summarize("Nothing in the last 30 days", "history")
            return False
        for day in days:
            note = ("A snapshot was taken this day — changes after it can be "
                    "undone from the boot menu." if day.snapshot else "")
            section = self._section(day.label, note)
            for e in day.events:
                section._rows.append(self._history_row(e))
            self.history_body.append(section)
        n = sum(len(d.events) for d in days)
        self._summarize(f"{n} change(s) in the last 30 days", "history")
        return False

    def _history_row(self, e):
        when = datetime.fromtimestamp(e.ts).strftime("%H:%M")
        subtitle = " · ".join(x for x in (when, e.detail) if x)
        if e.items:
            row = self._plain(Adw.ExpanderRow, title=e.title, subtitle=subtitle)
            for item in e.items[:80]:
                entry = self._plain(title=item)
                entry.add_css_class("property")
                row.add_row(entry)
        else:
            row = self._plain(title=e.title, subtitle=subtitle)
        icon = Gtk.Image(icon_name=("dialog-warning-symbolic" if e.severity == "warn"
                                    else self._HISTORY_ICONS.get(e.kind,
                                                                 "emblem-ok-symbolic")))
        icon.set_accessible_role(Gtk.AccessibleRole.PRESENTATION)
        row.add_prefix(icon)
        return row

    def _maint_reload(self):
        self._busy(True)
        self._summarize("Checking the system…", "maintenance")

        def work():
            try:
                findings = maintenance.run_all()
            except Exception as exc:                    # noqa: BLE001
                GLib.idle_add(self._say, f"Health checks failed: {exc}", 8)
                return
            GLib.idle_add(self._maint_show, findings)

        threading.Thread(target=work, daemon=True).start()

    def _maint_show(self, findings):
        self._busy(False)
        self._findings = findings
        while (row := self.maint_list.get_first_child()) is not None:
            self.maint_list.remove(row)

        for f in findings:
            self.maint_list.append(self._maint_row(f))

        issues = [f for f in findings if f.severity in
                  (maintenance.CRIT, maintenance.WARN)]
        free = maintenance.reclaimable_mb(findings)
        self._sync_primary()
        self._summarize(
            (f"{len(issues)} thing(s) worth attention · "
            f"{maintenance.fmt_size(free)} reclaimable"
            if issues else
            f"Nothing needs attention · {maintenance.fmt_size(free)} reclaimable"),
            "maintenance")

        # Keep the sidebar badge honest about what the page says.
        row = self.sidebar_rows.get("maintenance")
        if row is not None:
            crit = any(f.severity == maintenance.CRIT for f in findings)
            self._badge(row._count, len(issues), "crit" if crit else "warn")
        return False

    EMPTY_STATES = {
        "updates": ("emblem-ok-symbolic", "Everything is up to date",
                    "Nothing installed has a newer version waiting."),
        inventory.ORPHAN: ("emblem-ok-symbolic", "No orphans",
                           "Every installed package is either asked for "
                           "directly or needed by something that is."),
        inventory.FLATPAK: ("package-x-generic-symbolic", "No Flatpaks",
                            "Nothing is installed through Flatpak on this "
                            "machine."),
        inventory.APPIMAGE: ("application-x-executable-symbolic",
                             "No AppImages tracked",
                             "Install one from the App Store, or add an "
                             "existing file with ☰ → Track an AppImage."),
        inventory.LOCAL: ("applications-engineering-symbolic",
                          "No local builds",
                          "Nothing here is running from a git checkout."),
        inventory.AUR: ("package-x-generic-symbolic", "No AUR packages",
                        "Nothing foreign is installed."),
    }

    _SEVERITY_CSS = {
        maintenance.CRIT: ("Critical", "pill-crit"),
        maintenance.WARN: ("Attention", "pill-warn"),
        maintenance.INFO: ("Optional", "pill"),
        maintenance.OK: ("OK", "pill-ok"),
    }

    def _maint_row(self, f):
        label, css = self._SEVERITY_CSS.get(f.severity, ("", "pill"))

        if f.items:
            row = self._plain(Adw.ExpanderRow, title=f.title, subtitle=f.detail)
            for item in f.items[:60]:
                entry = self._plain(title=item)
                entry.add_css_class("property")
                entry.set_subtitle_selectable(True)
                row.add_row(entry)
            if len(f.items) > 60:
                row.add_row(self._plain(
                    title=f"… and {len(f.items) - 60} more"))
        else:
            row = self._plain(title=f.title, subtitle=f.detail)
        row.set_subtitle_lines(3)

        pill = Gtk.Label(label=label, valign=Gtk.Align.CENTER)
        pill.add_css_class(css)
        row.add_prefix(pill)

        if f.size_mb and f.id != "disk":
            size = Gtk.Label(label=maintenance.fmt_size(f.size_mb),
                             valign=Gtk.Align.CENTER)
            size.add_css_class("dim-label")
            size.add_css_class("caption")
            row.add_suffix(size)

        if f.actionable and f.fix_label:
            btn = Gtk.Button(label=f.fix_label, valign=Gtk.Align.CENTER)
            if f.severity == maintenance.CRIT:
                btn.add_css_class("suggested-action")
            btn.connect("clicked", lambda _b, f=f: self._maint_fix(f))
            row.add_suffix(btn)
        elif f.fix_note and not f.fix:
            # No button, but say what the human would have to do.
            hint = Gtk.Label(label="manual", valign=Gtk.Align.CENTER)
            hint.add_css_class("dim-label")
            hint.add_css_class("caption")
            hint.set_tooltip_text(f.fix_note)
            row.add_suffix(hint)
        return row

    def _maint_fix(self, f):
        detail = f.fix_note
        if f.items:
            detail += "\n\n" + "\n".join(f.items[:40])
            if len(f.items) > 40:
                detail += f"\n… and {len(f.items) - 40} more"
        destructive = f.id in ("trash", "orphans", "dangling-desktop",
                               "broken-symlinks", "rollbacks")
        tasks.confirm(
            self, f.title, f.detail, detail,
            f.fix_label, destructive,
            lambda: tasks.run(self, f.title, [(f.fix_label, f.fix)],
                              self._findings_done([f])))

    def _findings_done(self, fixed: list):
        """After fixes: ask each check again, and say what actually changed."""
        def done(ok):
            self._maint_after(ok)

            def work():
                outs = [verify.judge_finding(f, verify.recheck(f.id))
                        for f in fixed]
                failed = [o for o in outs if o.changed is False]
                for o in failed:
                    GLib.idle_add(self._report_outcome, o)
                if not failed and outs:
                    GLib.idle_add(self._say,
                                  " · ".join(o.message for o in outs), 8)
            threading.Thread(target=work, daemon=True).start()
            return False
        return done

    def _maint_after(self, ok):
        self._maint_reload()
        if ok:
            self.reload()
        return False

    def _reclaim_space(self):
        """The three cleanups that only ever free cached or logged data."""
        wanted = ("pacman-cache", "yay-cache", "journal")
        steps, detail, total = [], [], 0.0
        for f in getattr(self, "_findings", []):
            if f.id in wanted and f.actionable:
                steps.append((f"{f.title}: {f.fix_label}", f.fix))
                detail.append(f"{f.title:22} {maintenance.fmt_size(f.size_mb):>9}"
                              f"   {f.fix_note}")
                total += f.size_mb
        if not steps:
            self._say("Nothing to reclaim")
            return
        tasks.confirm(
            self, f"Reclaim about {maintenance.fmt_size(total)}?",
            "These free cached downloads, build artefacts and old logs. "
            "Nothing you created is touched, and nothing installed is removed.",
            "\n\n".join(detail), "Reclaim", False,
            lambda: tasks.run(self, "Reclaiming space", steps,
                              self._findings_done(
                                  [f for f in getattr(self, "_findings", [])
                                   if f.id in wanted and f.actionable])))

    # ---- the AI estate ----------------------------------------------------
    def _build_ai(self):
        """Harnesses, MCP servers, skills, plugins, marketplaces and hooks.

        Only the harnesses have versions, so only they belong in the package
        table. The rest is still software that rots -- an MCP server that
        stopped authenticating, a hook whose script was deleted, a marketplace
        three weeks stale -- and this is where that shows.
        """
        page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)

        self.ai_page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=20,
                               margin_start=12, margin_end=12, margin_bottom=16)
        scroller = Gtk.ScrolledWindow(vexpand=True)
        scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroller.set_child(self.ai_page)
        page.append(scroller)
        return page

    @staticmethod
    def _status_page(icon: str, title: str, body: str, action=None):
        """What a screen says when it has nothing to show.

        A blank list is indistinguishable from a broken one. Every empty
        screen here names which of the three it is -- nothing to do, nothing
        matched, or nothing could be loaded -- and offers the way out.
        """
        page = Adw.StatusPage(icon_name=icon, title=title, description=body)
        page.set_vexpand(True)
        if action is not None:
            page.set_child(action)
        return page

    @staticmethod
    def _labelled(label: str, widget):
        """A control with its name above it, for the filter drawer."""
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        cap = Gtk.Label(label=label, xalign=0)
        cap.add_css_class("caption")
        cap.add_css_class("dim-label")
        box.append(cap)
        widget.set_hexpand(True)
        box.append(widget)
        return box

    def _store_reset_filters(self):
        for widget in (self.store_cat, self.store_trust, self.store_market):
            widget.set_selected(0)
        self.store_sort.set_selected(0)
        self.store_verified.set_active(False)
        self.store_installed.set_active(False)
        self.search.set_text("")
        self._store_refresh()

    @staticmethod
    def _section(title: str, description: str = "", trailing=None):
        """One titled, full-width list. The shared shape for secondary pages."""
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        head = Gtk.Label(label=title, xalign=0, hexpand=True)
        head.add_css_class("heading")
        if trailing is not None:
            row = Gtk.Box(spacing=8)
            row.append(head)
            row.append(trailing)
            box.append(row)
        else:
            box.append(head)
        if description:
            sub = Gtk.Label(label=description, xalign=0, wrap=True)
            sub.add_css_class("dim-label")
            sub.add_css_class("caption")
            box.append(sub)
        rows = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
        rows.add_css_class("boxed-list")
        box.append(rows)
        box._rows = rows
        return box

    def _ai_reload(self):
        """Render each group as it arrives rather than waiting for all six.

        Checking MCP health means opening a connection to every configured
        server, which takes ten to twenty seconds on a cold cache. The skills
        and hooks are read from disk in milliseconds and should not be held
        behind it.
        """
        self._busy(True)
        self._summarize("Reading the AI setup…", inventory.AGENT)

        import demo
        if demo.active():
            # One shot, no threads: the fixtures are already in memory.
            self._ai_show(ai.survey(), final=True)
            self._busy(False)
            return

        specs = ai.group_specs()
        self._ai_survey = [(title, desc, []) for title, desc, _fn in specs]
        self._ai_pending = len(specs)
        self._ai_show(self._ai_survey, final=False)

        for index, (title, desc, fn) in enumerate(specs):
            self._thumb_pool.submit(
                lambda i=index, t=title, f=fn: GLib.idle_add(
                    self._ai_group_done, i, t, ai.load_group(f, t)))

    def _ai_group_done(self, index, title, entries):
        if index < len(self._ai_survey):
            t, desc, _old = self._ai_survey[index]
            self._ai_survey[index] = (t, desc, entries)
        self._ai_pending -= 1
        if self._ai_pending <= 0:
            self._busy(False)
        self._ai_show(self._ai_survey, final=self._ai_pending <= 0)
        return False

    _AI_SEVERITY = {
        "warn": ("Attention", "pill-warn"),
        "info": ("Optional", "pill"),
        "ok": ("", ""),
    }

    def _ai_show(self, groups, final=True):
        # Keep what was rendered: revisiting the page re-renders from this
        # rather than reloading, and an unrecorded result showed as empty.
        if final:
            self._ai_survey = groups
        while (child := self.ai_page.get_first_child()) is not None:
            self.ai_page.remove(child)

        loading = not final
        for index, (title, desc, entries) in enumerate(groups):
            pending = loading and not entries
            section = self._section(
                f"{title} …" if pending else f"{title} ({len(entries)})", desc)
            if pending:
                row = self._plain(title="Checking…")
                spinner = Gtk.Spinner(valign=Gtk.Align.CENTER)
                spinner.start()
                row.add_prefix(spinner)
                section._rows.append(row)
            elif not entries:
                section._rows.append(self._plain(
                    title="None", subtitle="Nothing of this kind is set up."))
            # Trouble first: a broken hook among thirty healthy skills is the
            # whole reason to open this page.
            order = {"warn": 0, "info": 1, "ok": 2, "": 2}
            for e in sorted(entries, key=lambda e: (order.get(e.severity, 2),
                                                    e.name.lower())):
                section._rows.append(self._ai_row(e))
            self.ai_page.append(section)

        self._sync_primary()
        total = sum(len(es) for _t, _d, es in groups)
        attention = ai.needs_attention(groups)
        if loading:
            self._summarize(f"{total} found so far…", inventory.AGENT)
        else:
            self._summarize(
                f"{total} items across {len(groups)} kinds · "
                f"{attention} need attention" if attention else
                f"{total} items across {len(groups)} kinds · all healthy",
                inventory.AGENT)

        row = self.sidebar_rows.get(inventory.AGENT)
        if row is not None and final:
            self._badge(row._count, attention or total,
                        "warn" if attention else "")
        return False

    def _ai_row(self, e):
        subtitle = e.detail
        if e.version:
            subtitle = (f"{e.version}" + (f" → {e.update}" if e.update else "")
                        + (f" · {e.detail}" if e.detail else ""))
        row = self._plain(title=e.name, subtitle=subtitle)
        row.set_subtitle_lines(2)
        row.set_subtitle_selectable(True)

        # The vendor's own mark, fetched from the vendor. Falls back to a
        # symbolic icon immediately and swaps in the real one when it arrives,
        # so a slow or missing favicon never holds up the row.
        art = Gtk.Image(pixel_size=22, valign=Gtk.Align.CENTER)
        art.set_from_icon_name(e.fallback_icon)
        art.add_css_class("store-shot")
        row.add_prefix(art)
        self._vendor_icon(art, e)

        label, css = self._AI_SEVERITY.get(e.severity, ("", ""))
        if label:
            pill = Gtk.Label(label=label, valign=Gtk.Align.CENTER)
            pill.add_css_class(css)
            row.add_prefix(pill)

        if e.status:
            st = Gtk.Label(label=e.status, valign=Gtk.Align.CENTER)
            st.add_css_class("dim-label")
            st.add_css_class("caption")
            row.add_suffix(st)

        if e.url:
            link = Gtk.Button(icon_name="adw-external-link-symbolic",
                              valign=Gtk.Align.CENTER)
            link.add_css_class("flat")
            link.set_tooltip_text(e.url)
            link.connect("clicked", lambda _b, u=e.url: self.open_url(u))
            row.add_suffix(link)

        if e.action_label and e.action is not None:
            btn = Gtk.Button(label=e.action_label, valign=Gtk.Align.CENTER)
            if e.severity == "warn":
                btn.add_css_class("suggested-action")
            btn.connect("clicked", lambda _b, e=e: self._ai_act(e))
            row.add_suffix(btn)
        elif e.action_note and e.action is None and e.action_label:
            # There is something to do, but not something this app can do.
            hint = Gtk.Label(label="manual", valign=Gtk.Align.CENTER)
            hint.add_css_class("dim-label")
            hint.add_css_class("caption")
            hint.set_tooltip_text(e.action_note)
            row.add_suffix(hint)
        return row

    def _app_icon(self, image, app, size: int) -> bool:
        """The best icon available for an inventory row.

        Order: the .desktop icon, then the vendor's own mark where we know
        their domain, then a per-source glyph. Agents have no .desktop entry,
        so without the middle step Claude Code and Codex rendered as the same
        grey circle as an unidentifiable package.

        Returns False when it fell through to the glyph, so the caller can
        dim it.
        """
        icon = app.extra.get("icon") or ""
        theme = Gtk.IconTheme.get_for_display(self.get_display())
        if icon and theme.has_icon(icon):
            image.set_from_icon_name(icon)
            return True
        if icon and Path(icon).is_file():
            image.set_from_file(icon)
            return True

        key, domain, url = self._icon_source(app)
        if key:
            hit = icons_mod.cached(key)
            if hit:
                self._set_icon(image, hit, size)
                return True
            # Not fetched yet: show the glyph now, swap it in when it lands.
            image.set_from_icon_name(
                _FALLBACK_ICON.get(app.source, "package-x-generic"))
            self._thumb_pool.submit(
                lambda: GLib.idle_add(
                    self._icon_arrived, image, size,
                    icons_mod.fetch(key, url, domain)))
            return True

        image.set_from_icon_name(
            _FALLBACK_ICON.get(app.source, "package-x-generic"))
        return False

    @staticmethod
    def _icon_source(app):
        """(cache key, domain, explicit url) for an entry's vendor mark.

        pacman already records every package's upstream homepage, so most of
        the list can show the project's own logo instead of a generic box.
        Projects hosted on GitHub are the exception worth special-casing: the
        site favicon would put the same octocat on half the list, while the
        owner's avatar is the thing people actually recognise.
        """
        domain = app.extra.get("domain") or ""
        if domain:
            return domain, domain, ""

        # Local builds and plugins record their clone URL rather than a
        # homepage; Flatpaks are named after their vendor's domain in
        # reverse. Both are an upstream identity, which is all this needs.
        url = app.extra.get("url", "") or app.extra.get("repo_url", "")
        if not url and app.extra.get("appid"):
            parts = app.extra["appid"].split(".")
            if len(parts) >= 2:
                return ".".join(reversed(parts[:2])), ".".join(reversed(parts[:2])), ""
        host = icons_mod.host_of(url)
        if not host:
            return "", "", ""
        if host.endswith("github.com"):
            parts = [x for x in url.split("/") if x]
            owner = parts[2] if len(parts) > 2 else ""
            if owner and owner not in ("sponsors", "orgs"):
                return f"gh-{owner.lower()}", "", icons_mod.github_avatar(owner)
            return "", "", ""
        if host.endswith("gitlab.com") or host.endswith("codeberg.org"):
            return icons_mod.registrable(host), host, ""
        return icons_mod.registrable(host), host, ""

    def _icon_arrived(self, image, size, path):
        if path:
            self._set_icon(image, path, size)
        return False

    def _vendor_icon(self, image, e):
        if not (e.icon_key and (e.icon_domain or e.icon_url)):
            return
        hit = icons_mod.cached(e.icon_key)
        if hit:
            self._set_icon(image, hit)
            return

        def done(path):
            if path:
                self._set_icon(image, path)
            return False

        self._thumb_pool.submit(
            lambda: GLib.idle_add(
                done, icons_mod.fetch(e.icon_key, e.icon_url, e.icon_domain)))

    def _set_icon(self, image, path, size: int = 48):
        tex = self._texture(path, size)
        if tex is not None:
            image.set_from_paintable(tex)

    def _ai_act(self, e):
        cmd = e.action
        if (e.kind == ai.HARNESS and e.update and isinstance(cmd, list)
                and agents_mod.major_jump(e.version, e.update)):
            # Same treatment as from the Updates list: notes first.
            spec = next((k for k in agents_mod.KNOWN if k.name == e.name), None)
            app = inventory.App(name=e.name, version=e.version, source=inventory.AGENT,
                                update=e.update,
                                extra={"update_cmd": cmd, "manager": "",
                                       "repo": spec.repo if spec else "",
                                       "binary": spec.id if spec else ""})
            app.extra["manager"] = "mise" if cmd[:1] == ["mise"] else "self"
            if app.extra["manager"] == "mise" and spec:
                app.extra["key"] = spec.id
            self._update_agent(app)
            return
        detail = e.action_note or ""
        if isinstance(cmd, list):
            detail += f"\n\n$ {' '.join(cmd)}"
        tasks.confirm(
            self, f"{e.action_label} {e.name}?", e.detail or "", detail.strip(),
            e.action_label, False,
            lambda: tasks.run(self, f"{e.action_label} {e.name}",
                              [(f"{e.action_label} {e.name}", cmd)],
                              lambda ok: self._ai_after(ok)))

    def _ai_after(self, ok):
        self._ai_reload()
        if ok:
            self.reload()
        return False

    def _ai_update_all(self):
        upd = [e for _t, _d, es in getattr(self, "_ai_survey", [])
               for e in es if e.kind == ai.HARNESS and e.update and e.action]
        # Major releases are updated one at a time, with their notes shown.
        held = [e for e in upd if agents_mod.major_jump(e.version, e.update)]
        upd = [e for e in upd if e not in held]
        if held:
            self._say("Held back (major release, update it on its own): "
                      + ", ".join(f"{e.name} {e.update}" for e in held), 8)
        if not upd:
            return
        steps = [(f"Updating {e.name}", e.action) for e in upd]
        detail = "\n".join(
            f"{e.name:22} {e.version} → {e.update}\n  $ {' '.join(e.action)}"
            for e in upd)
        tasks.confirm(
            self, f"Update {len(upd)} agent CLI(s)?",
            "Each replaces its own binary. None of this needs root, and none "
            "of it touches your agent configuration.",
            detail, "Update", False,
            lambda: tasks.run(self, "Updating agent CLIs", steps,
                              lambda ok: self._ai_after(ok)))

    # ---- settings ---------------------------------------------------------
    def on_preferences(self, *_a):
        """Scheduled updates: what runs unattended, and how often."""
        cfg = autoupdate.load()
        st = autoupdate.status()

        dialog = Adw.PreferencesDialog(title="Settings")
        page = Adw.PreferencesPage(title="Updates",
                                   icon_name="software-update-available-symbolic")

        sched = Adw.PreferencesGroup(
            title="Automatic updates",
            description="A systemd user timer runs the update in the "
                        "background. Anything that could stop the machine "
                        "booting is always held for you to run by hand, "
                        "whatever is set here.")

        enabled = Adw.SwitchRow(title="Run updates on a schedule",
                                active=st["enabled"])
        sched.add(enabled)

        labels = [label for _k, label, _c in autoupdate.SCHEDULES]
        keys = [k for k, _l, _c in autoupdate.SCHEDULES]
        current = autoupdate.current_schedule()
        combo = Adw.ComboRow(title="How often",
                             model=Gtk.StringList.new(labels))
        combo.set_selected(keys.index(current) if current in keys else 1)
        sched.add(combo)

        next_row = Adw.ActionRow(title="Next run", subtitle=st["next"])
        next_row.set_subtitle_selectable(True)
        sched.add(next_row)
        last_row = Adw.ActionRow(title="Last run", subtitle=st["last"])
        last_row.set_subtitle_selectable(True)
        sched.add(last_row)
        page.add(sched)

        include = Adw.PreferencesGroup(
            title="What the scheduled run updates",
            description="Each of these updates independently. Turning one off "
                        "does not stop you updating it by hand here.")
        rows = {}
        for key, title, subtitle in (
                ("packages", "Repository and AUR packages",
                 "Needs passwordless sudo to apply; without it the run only "
                 "notifies you that updates are waiting."),
                ("agents", "AI agent CLIs",
                 "Claude Code, Codex and the rest. They replace their own "
                 "binaries and need no root."),
                ("appimages", "Tracked AppImages",
                 "Downloads the newer build and keeps the previous one so an "
                 "update can be reverted."),
                ("plugins", "Omarchy shell plugins",
                 "Off by default: a plugin update changes the desktop while "
                 "you are using it.")):
            row = Adw.SwitchRow(title=title, subtitle=subtitle,
                                active=bool(cfg.get(key)))
            rows[key] = row
            include.add(row)
        page.add(include)

        # The settings watcher, for while this window is closed.
        watch = Adw.PreferencesGroup(
            title="While Almanac is closed",
            description="A small background process with no window. It only "
                        "watches config folders, and notifies you the moment "
                        "a tool resets one of your files to its defaults.")
        guard_row = Adw.SwitchRow(
            title="Watch for settings reset to defaults",
            subtitle="Starts with your session, through the standard autostart "
                     "folder",
            active=watcher.enabled())

        def toggle_guard(row, _pspec):
            try:
                watcher.enable() if row.get_active() else watcher.disable()
            except OSError as exc:
                self._say(f"Could not change the autostart entry: {exc}")
        guard_row.connect("notify::active", toggle_guard)
        watch.add(guard_row)
        page.add(watch)

        actions = Adw.PreferencesGroup()
        run_row = Adw.ActionRow(
            title="Run the update now",
            subtitle="Runs exactly what the schedule would run, in a window "
                     "you can watch.")
        run_btn = Gtk.Button(label="Run now", valign=Gtk.Align.CENTER)
        run_btn.set_tooltip_text(
            "Run the scheduled update immediately, in a window you can watch")
        run_btn.add_css_class("suggested-action")
        run_btn.connect("clicked", lambda _b: self._run_scheduled_now())
        run_row.add_suffix(run_btn)
        actions.add(run_row)

        log_row = Adw.ActionRow(
            title="Last run log",
            subtitle=str(Path.home() / ".local/state/omarchy-auto-update/last-run.log"))
        log_row.set_subtitle_selectable(True)
        actions.add(log_row)
        page.add(actions)

        dialog.add(page)

        # Applied on close rather than per-keystroke: flipping four switches
        # should not mean four daemon-reloads.
        def apply(*_a):
            new = dict(cfg)
            for key, row in rows.items():
                new[key] = row.get_active()
            want = keys[combo.get_selected()]
            new["schedule"] = want
            autoupdate.save(new)
            if want != current:
                ok, msg = autoupdate.set_schedule(want)
                if not ok:
                    self._say(f"Could not change the schedule: {msg}")
            if enabled.get_active() != st["enabled"]:
                ok, msg = autoupdate.set_enabled(enabled.get_active())
                if not ok:
                    self._say(f"Could not change the timer: {msg}")
            state = "on" if enabled.get_active() else "off"
            self._say(f"Scheduled updates {state}")

        dialog.connect("closed", apply)
        dialog.present(self)

    def _run_scheduled_now(self):
        tasks.run(self, "Scheduled update",
                  [("Running the scheduled update",
                    [str(Path.home() / ".local/bin/omarchy-auto-update")])],
                  self._after_pacman)

    # ---- featured ---------------------------------------------------------
    def _build_featured(self):
        """The landing page: what deserves attention, assembled from live data."""
        page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self.feat_page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=20,
                                 margin_start=12, margin_end=12,
                                 margin_top=12, margin_bottom=16)
        self.feat_page.append(self._build_hero())
        scroller = Gtk.ScrolledWindow(vexpand=True)
        scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroller.set_child(self.feat_page)
        page.append(scroller)
        return page

    HERO_HEIGHT = 190

    def _build_hero(self):
        """The machine's own wallpaper, with the state of the machine on it.

        A hero that is decoration is dead weight on a page about what needs
        doing. This one is the image already behind the window — so it costs
        no download, carries no stock art, and re-skins with the theme — and
        it carries the one line that says whether anything is wrong.
        """
        overlay = Gtk.Overlay()
        overlay.set_size_request(-1, self.HERO_HEIGHT)
        overlay.add_css_class("hero")
        overlay.set_overflow(Gtk.Overflow.HIDDEN)

        self.hero_pic = Gtk.Picture()
        self.hero_pic.set_content_fit(Gtk.ContentFit.COVER)
        self.hero_pic.set_hexpand(True)
        self.hero_pic.set_vexpand(True)
        clamp = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
        clamp.set_policy(Gtk.PolicyType.EXTERNAL, Gtk.PolicyType.EXTERNAL)
        clamp.set_propagate_natural_width(False)
        clamp.set_propagate_natural_height(False)
        clamp.set_child(self.hero_pic)
        overlay.set_child(clamp)

        scrim = Gtk.Box()
        scrim.add_css_class("hero-scrim")
        overlay.add_overlay(scrim)

        text = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4,
                       valign=Gtk.Align.END, halign=Gtk.Align.START,
                       margin_start=20, margin_end=20, margin_bottom=16)
        self.hero_head = Gtk.Label(xalign=0, wrap=True)
        self.hero_head.add_css_class("hero-head")
        self.hero_sub = Gtk.Label(xalign=0, wrap=True)
        self.hero_sub.add_css_class("hero-sub")
        text.append(self.hero_head)
        text.append(self.hero_sub)
        overlay.add_overlay(text)

        self.hero = overlay
        self._refresh_hero()
        return overlay

    def _refresh_hero(self):
        """Point the hero at the current wallpaper, or hide it if there is none."""
        bg = theming.background()
        if bg is None:
            self.hero.set_visible(False)
            return
        self.hero.set_visible(True)
        tex = self._texture(bg, 1200)
        if tex is not None:
            self.hero_pic.set_paintable(tex)

    def _hero_text(self, problems: int):
        import datetime
        hour = datetime.datetime.now().hour
        part = ("Good morning" if hour < 12
                else "Good afternoon" if hour < 18 else "Good evening")
        name = theming.theme_name()
        if problems:
            self.hero_head.set_text(f"{problems} thing{'s' if problems != 1 else ''} need you")
        else:
            self.hero_head.set_text(f"{part} — nothing needs you")
        bits = []
        updates = sum(1 for i in range(self.store.get_n_items())
                      if self.store.get_item(i).has_update)
        if updates:
            bits.append(f"{updates} update{'s' if updates != 1 else ''} available")
        free = maintenance.reclaimable_mb(getattr(self, "_findings", []) or [])
        if free:
            bits.append(f"{maintenance.fmt_size(free)} reclaimable")
        if name:
            bits.append(f"wearing {name}")
        self.hero_sub.set_text(" · ".join(bits))

    def _feat_reload(self):
        self._busy(True)
        self._summarize("Working out what needs you…", "featured")

        def work():
            apps = [self.store.get_item(i).app
                    for i in range(self.store.get_n_items())]
            findings = getattr(self, "_findings", None) or maintenance.run_all()
            plugins = getattr(self, "_plugins", None)
            if not plugins:
                try:
                    plugins = store_mod.catalog()
                    store_mod.annotate(plugins)
                    self._plugins = plugins
                except Exception:                       # noqa: BLE001
                    plugins = []
            self._findings = findings
            shelves = featured.build(apps, findings, plugins)
            GLib.idle_add(self._feat_show, shelves)

        threading.Thread(target=work, daemon=True).start()

    def _feat_show(self, shelves):
        self._busy(False)
        self._shelves = shelves
        # Keep the hero; replace everything under it.
        while (child := self.hero.get_next_sibling()) is not None:
            self.feat_page.remove(child)

        problems = 0
        for shelf in shelves:
            section = self._section(
                f"{shelf.title} ({len(shelf.items)})" if shelf.items else shelf.title,
                shelf.subtitle,
                self._shelf_link(shelf))
            if not shelf.items:
                row = self._plain(title=shelf.empty)
                if shelf.key == "attention":
                    tick = Gtk.Image(icon_name="emblem-ok-symbolic",
                                     valign=Gtk.Align.CENTER)
                    tick.add_css_class("store-verified")
                    row.add_prefix(tick)
                section._rows.append(row)
            for item in shelf.items:
                if item.severity in ("crit", "warn"):
                    problems += 1
                section._rows.append(self._feat_row(item))
            self.feat_page.append(section)

        self._refresh_hero()
        self._hero_text(problems)
        self._summarize(
            f"{problems} thing(s) need you" if problems
            else "Nothing needs you right now", "featured")
        row = self.sidebar_rows.get("featured")
        if row is not None:
            # Featured's shelves are Shelf objects; the AI page's are
            # (title, description, entries) tuples. Different shapes.
            crit = any(item.severity == "crit"
                       for shelf in shelves for item in shelf.items)
            self._badge(row._count, problems, "crit" if crit else "warn")
        return False

    # Each shelf is a sample of a bigger list; this says where the rest is.
    _SHELF_TARGET = {
        "attention": ("Open Health check", "maintenance", None),
        "new": ("Show all", "discover", "new"),
        "trending": ("Show all", "discover", "trending"),
        "affinity": ("Show all", "discover", "popular"),
    }

    def _shelf_link(self, shelf):
        target = self._SHELF_TARGET.get(shelf.key)
        if not target or not shelf.items:
            return None
        label, page, sort = target
        btn = Gtk.Button(label=label, valign=Gtk.Align.CENTER)
        btn.add_css_class("flat")
        btn.set_tooltip_text(
            f"Open the {'App Store' if page == 'discover' else 'Health check'}"
            + (f", sorted by {sort}" if sort else ""))
        btn.connect("clicked", lambda _b: self._open_shelf(page, sort))
        return btn

    def _open_shelf(self, page, sort):
        if page == "discover" and sort:
            keys = [k for k, _l, _f in store_mod.SORTS]
            if sort in keys:
                self.store_sort.set_selected(keys.index(sort))
        self._select_category(page)

    def _feat_row(self, item):
        row = self._plain(title=item.title, subtitle=item.detail)
        row.set_subtitle_lines(2)

        if item.severity:
            label, css = self._SEVERITY_CSS.get(
                maintenance.CRIT if item.severity == "crit" else maintenance.WARN,
                ("", "pill"))
            pill = Gtk.Label(label=label, valign=Gtk.Align.CENTER)
            pill.add_css_class(css)
            row.add_prefix(pill)

        art = Gtk.Image(pixel_size=24, valign=Gtk.Align.CENTER)
        if item.plugin is not None:
            art.set_from_icon_name("application-x-addon-symbolic")
            self._plugin_icon(art, item.plugin)
        elif item.app is not None:
            self._app_icon(art, item.app, 48)
        else:
            art.set_from_icon_name("emblem-important-symbolic")
            art.add_css_class("dim-label")
        row.add_prefix(art)

        if item.plugin is not None:
            chip = Gtk.Label(label=item.plugin.market_name,
                             valign=Gtk.Align.CENTER)
            chip.add_css_class("pill")
            row.add_suffix(chip)

        if item.action:
            btn = (Gtk.Button(icon_name=INSTALL_ICON, valign=Gtk.Align.CENTER)
                   if item.action == "Install" else
                   Gtk.Button(label=item.action, valign=Gtk.Align.CENTER))
            btn.update_property([Gtk.AccessibleProperty.LABEL],
                                [f"{item.action} {item.title}"])
            if item.severity == "crit":
                btn.add_css_class("suggested-action")
            btn.set_tooltip_text(f"{item.action} {item.title}")
            btn.connect("clicked", lambda _b, it=item: self._feat_act(it))
            row.add_suffix(btn)

        row.set_activatable(True)
        row.connect("activated", lambda _r, it=item: self._feat_open(it))
        return row

    def _plugin_icon(self, image, plugin):
        """A marketplace entry's own card art, scaled down to a row icon."""
        cached = store_mod.thumb_path(plugin)
        if cached is not None and cached.exists() and cached.stat().st_size:
            self._set_icon(image, cached, 64)
            return

        def done(path):
            if path:
                self._set_icon(image, path, 64)
            return False

        if plugin.thumb:
            self._thumb_pool.submit(
                lambda: GLib.idle_add(done, store_mod.fetch_thumb(plugin)))

    def _feat_act(self, item):
        if item.plugin is not None:
            self.on_plugin_install(None, item.plugin)
        elif item.kind == "update" and item.app is not None:
            self.on_update_one(None, item.app)
        elif item.kind == "finding":
            self._select_category("maintenance")

    def _feat_open(self, item):
        if item.plugin is not None:
            self._show_plugin(item.plugin)
        elif item.app is not None:
            self._select_category("updates")
        else:
            self._select_category("maintenance")

    # ---- actions ---------------------------------------------------------
    def _row_visit(self, name: str):
        app = self._row_actions.get(name)
        if app and app.extra.get("url"):
            self.open_url(app.extra["url"])

    def _row_notes(self, name: str):
        """Jump to the releases page, where the real changelog is."""
        app = self._row_actions.get(name)
        if not app:
            return
        url = app.extra.get("url", "")
        # github.com/owner/repo[/...] -> github.com/owner/repo/releases
        parts = url.split("github.com/", 1)[-1].strip("/").split("/")
        if len(parts) >= 2:
            self.open_url(f"https://github.com/{parts[0]}/{parts[1]}/releases")
        elif url:
            self.open_url(url)

    def on_self_update(self, *_a):
        """Update Almanac, then offer to restart into the new code."""
        info = selfupdate.check()
        if not info["can_update"]:
            self._say(info["note"] or "Almanac is up to date", 8)
            return
        tasks.confirm(
            self, f"Update Almanac?",
            f"{info['behind']} commit(s) behind. The update is a "
            f"fast-forward pull, so it cannot overwrite local work — if "
            f"there were any, this would refuse instead.",
            f"Checkout  {selfupdate.ROOT}\n"
            f"Remote    {info['remote']}\n"
            f"Newest    {info['latest']}\n\n"
            f"$ git pull --ff-only",
            "Update", False,
            lambda: tasks.run(self, "Updating Almanac",
                              selfupdate.update_steps(),
                              self._after_self_update))

    def _after_self_update(self, ok: bool):
        if not ok:
            self._say("Almanac was not updated", 8)
            return
        # The running process cannot adopt the new source, so say so rather
        # than letting the version in the list disagree with what is running.
        toast = Adw.Toast(title="Almanac updated — restart to use the new "
                                "version", timeout=0)
        toast.set_button_label("Restart")
        toast.connect("button-clicked", lambda *_: selfupdate.restart())
        self.toasts.add_toast(toast)
        self.reload()
        return False

    def on_about(self, *_a):
        info = selfupdate.check(fetch=False)
        about = Adw.AboutDialog(
            application_name="Almanac",
            application_icon="io.github.twodxme.Almanac",
            version=info["version"],
            developer_name="twodxme",
            comments="A catalogue of everything installed on this machine: "
                     "where each thing came from, how old it is, whether it "
                     "still has a maintainer, and what needs attention.",
            website="https://github.com/twodxme/almanac",
            issue_url="https://github.com/twodxme/almanac/issues",
            license_type=Gtk.License.MIT_X11)
        about.add_credit_section("Catalogues", [
            "Omarchy plugin marketplace https://plugins.omarchy.org",
            "ArtCraft https://getartcraft.com",
        ])
        about.set_debug_info(
            f"Installed as: {info['kind']}\n{info['note']}\n"
            + theming.describe() + "\n" + ghcache.describe())
        notes = selfupdate.release_notes()
        if notes:
            version, markup = notes
            about.set_release_notes_version(version)
            about.set_release_notes(markup)
        about.present(self)

    def _row_folder(self, name: str):
        app = self._row_actions.get(name)
        if app and app.extra.get("repo"):
            self.open_url(f"file://{app.extra['repo']}")

    def _row_changes(self, name: str):
        """Show what is uncommitted, read-only, before offering to move it."""
        app = self._row_actions.get(name)
        repo = app.extra.get("repo") if app else ""
        if not repo:
            return
        tasks.run(self, f"Local changes in {name}",
                  [("Working tree", ["git", "-C", repo, "status", "--short"]),
                   ("Diff summary", ["git", "-C", repo, "diff", "--stat"])])

    def _row_pull(self, name: str):
        app = self._row_actions.get(name)
        if app:
            self.on_git_pull(None, app)

    def _row_stash(self, name: str):
        """Update a checkout that has local edits -- see on_git_pull."""
        app = self._row_actions.get(name)
        if app:
            self.on_git_pull(None, app)

    def _row_aur(self, name: str):
        self.open_url(f"https://aur.archlinux.org/packages/{name}")

    def _row_remove(self, name: str):
        app = self._row_actions.get(name)
        if app:
            self.on_remove(None, app)

    def open_url(self, url: str):
        """Hand the URL to the desktop's default browser."""
        try:
            Gio.AppInfo.launch_default_for_uri(url, None)
        except GLib.Error as e:
            self._say(f"Could not open {url}: {e.message}")

    def on_launch(self, _button, app: inventory.App):
        try:
            subprocess.Popen([app.exec_path], start_new_session=True)
        except OSError as e:
            self._say(f"Could not launch {app.name}: {e}")

    def _in_terminal(self, script: str, note: str):
        """The one remaining escape to a terminal: see on_update_all."""
        argv = xdg.terminal_argv(
            ["bash", "-lc",
             script + "; echo; read -n1 -r -p 'Done — press any key to close'"])
        if not argv:
            self._say("No terminal found to run that in")
            return False
        try:
            subprocess.Popen(argv, start_new_session=True)
            self._say(note)
            return True
        except OSError as e:
            self._say(f"Could not start a terminal: {e}")
            return False

    def on_clean_orphans(self, _button):
        """Remove only the orphans judged safe, with pacman asking first.

        Build tooling and debug packages are excluded: the next AUR build pulls
        rust straight back, so removing it is churn rather than cleanup.
        """
        safe, kept = [], []
        for i in range(self.store.get_n_items()):
            it = self.store.get_item(i)
            if it.source != inventory.ORPHAN:
                continue
            (kept if it.advice == "keep" else safe).append(it.name)

        if not safe:
            msg = "No orphans worth removing"
            if kept:
                msg += f" — {len(kept)} kept deliberately (build tooling, debug packages)"
            self._say(msg)
            return

        ok, preview = tasks.preview_removal(safe)
        if not ok:
            self._say(f"Could not work out what would be removed: {preview}")
            return
        kept_note = (f"  {len(kept)} kept deliberately (build tooling, debug packages)"
                     if kept else "")
        tasks.confirm(
            self, f"Remove {len(safe)} orphaned package(s)?",
            "These are installed as dependencies but nothing needs them any "
            "more. Everything that would go is listed below." + kept_note,
            preview, "Remove", True,
            lambda: tasks.run(
                self, "Removing orphans",
                [("Removing orphaned packages",
                  ["pacman", "-Rns", "--noconfirm", *safe])],
                self._after_pacman))

    def on_install_item(self, _button, app: inventory.App):
        """Install one result, through the tool that owns that kind of thing."""
        if app.source == inventory.PLUGIN:
            url = app.extra.get("repo", "")
            if not url:
                return
            tasks.confirm(
                self, f"Install the {app.name} plugin?",
                "Omarchy shell plugins run inside the shell process with your "
                "permissions — there is no sandbox.",
                f"Source: {url}\n\n$ omarchy plugin add {url} --enable --yes",
                "Install", False,
                lambda: tasks.run(
                    self, f"Installing {app.name}",
                    [(f"Installing {app.name}",
                      ["omarchy", "plugin", "add", url, "--enable", "--yes"])],
                    self._after_pacman))
            return

        # yay builds as this user and only needs root for the final pacman
        # call; --sudo pkexec is what lets that happen without a terminal.
        argv = ["yay", "-S", "--needed", "--noconfirm",
                "--sudo", "pkexec", app.name]
        tasks.confirm(
            self, f"Install {app.name}?",
            f"From {SOURCE_LABEL.get(app.source, app.source)}. "
            f"AUR packages are built from source on this machine.",
            f"{app.name} {app.version}\n{app.description}\n\n"
            f"$ {' '.join(argv)}",
            "Install", False,
            lambda: self._install_package(app.name, argv))

    def on_install(self, _button):
        """Search and install, handed to yay in a terminal.

        yay's own interactive picker is better than anything reimplemented
        here, and it is the tool that owns the transaction.
        """
        text = self.search.get_text().strip()
        if not text:
            self.search.grab_focus()
            self._say("Type what to search for, then press Install")
            return
        if sources.parse(text):
            self.on_install_link(link=text)
            return
        argv = ["yay", "-S", "--needed", "--noconfirm", "--sudo", "pkexec", text]
        tasks.confirm(
            self, f"Install “{text}”?",
            "This installs a package by exact name. To browse what matches "
            "first, use the App Store and search there.",
            f"$ {' '.join(argv)}", "Install", False,
            lambda: self._install_package(text, argv))

    def _install_package(self, name: str, argv: list):
        """Install through yay: snapshot first, then check what changed."""
        holder = self._watch_defaults()

        def after(ok):
            if ok:
                self._check_defaults(holder)
            self._after_pacman(ok)

        tasks.run(self, f"Installing {name}",
                  self._snapshot_first([(f"Installing {name}", argv)],
                                       f"installing {name}"),
                  self._checked(verify.installed_package(name),
                                lambda b, a: verify.Outcome(
                                    bool(a) and a != b,
                                    f"{name} {a} installed" if a and a != b
                                    else f"{name} was not installed"),
                                then=after))

    def on_remove(self, _button, app: inventory.App):
        """Remove a package, with its orphaned dependencies.

        -Rns is what a user means by "uninstall", but it can take more than
        expected, so the terminal shows the full list before confirming --
        no --noconfirm here, deliberately.
        """
        if app.source == inventory.APPIMAGE:
            self._remove_appimage(app)
            return
        if app.source == inventory.FLATPAK:
            appid = app.extra.get("appid", app.name)
            tasks.confirm(
                self, f"Remove {app.name}?", "The Flatpak and its data are removed.",
                f"$ flatpak uninstall --assumeyes {appid}", "Remove", True,
                lambda: tasks.run(self, f"Removing {app.name}",
                                  [(f"Removing {app.name}",
                                    ["flatpak", "uninstall", "--assumeyes", appid])],
                                  self._after_pacman))
            return

        ok, preview = tasks.preview_removal([app.name])
        if not ok:
            self._say(f"Cannot remove {app.name}: {preview}")
            return
        count = len([l for l in preview.splitlines() if l.strip()])
        tasks.confirm(
            self, f"Remove {app.name}?",
            f"-Rns also takes dependencies nothing else needs. "
            f"{count} package(s) would be removed, listed below. "
            f"Configuration files are not kept.",
            preview, "Remove", True,
            lambda: tasks.run(self, f"Removing {app.name}",
                              [(f"Removing {app.name}",
                                ["pacman", "-Rns", "--noconfirm", app.name])],
                              self._after_pacman))

    # ---- did it work? ------------------------------------------------------
    def _checked(self, probe, judge, then=None):
        """An on_done that reports what changed, not what a command exited.

        `probe` is asked now and again once the task is over, and `judge`
        turns the two answers into what the user is told (see verify.py).
        Every silent failure this app has had passed an exit-status check.
        """
        before = probe() if probe else None

        def done(ok):
            if then is not None:
                then(ok)
            if probe is None:
                return False

            def work():
                GLib.idle_add(self._report_outcome, judge(before, probe()))
            threading.Thread(target=work, daemon=True).start()
            return False
        return done

    def _report_outcome(self, out):
        history.record(history.ALMANAC, out.message,
                       severity="warn" if out.changed is False else "")
        if out.changed is False:
            # Something the user asked for did not happen. A toast can be
            # missed; this is the one result that must not be.
            self._notify("Nothing changed", out.message)
            self._say(out.message, 10)
        else:
            self._say(out.message)
        return False

    def _snapshot_first(self, steps: list, what: str) -> list:
        """Put a Snapper snapshot ahead of a package change, when available."""
        step = snapshots.create_step(f"Almanac: before {what}")
        return [step, *steps] if step else list(steps)

    # ---- which files an install took over ----------------------------------
    def _watch_defaults(self) -> dict:
        """Read every file type's default app now, before an install."""
        holder: dict = {}

        def work():
            holder["before"] = mimeguard.snapshot()
        holder["thread"] = threading.Thread(target=work, daemon=True)
        holder["thread"].start()
        return holder

    def _check_defaults(self, holder: dict):
        def work():
            holder["thread"].join(timeout=90)
            moved = mimeguard.changes(holder.get("before"), mimeguard.snapshot())
            if moved:
                GLib.idle_add(self._offer_defaults_back, moved)
        threading.Thread(target=work, daemon=True).start()

    def _offer_defaults_back(self, moved):
        returnable = [m for m in moved if m[1]]
        dialog = Adw.AlertDialog(
            heading="Some files now open in a different app",
            body=("\n".join(mimeguard.summary(moved))
                  + ("\n\nGive them back to what opened them before?"
                     if returnable else "")))
        dialog.add_response("keep", "Keep the new app")
        if returnable:
            dialog.add_response("restore", "Give them back")
            dialog.set_response_appearance(
                "restore", Adw.ResponseAppearance.SUGGESTED)
        dialog.set_default_response("keep")
        dialog.set_close_response("keep")

        def respond(_d, resp):
            if resp == "restore":
                n = mimeguard.restore(returnable)
                self._say(f"{n} file type(s) handed back")
                history.record(history.ALMANAC,
                               f"{n} file type(s) handed back",
                               items=mimeguard.summary(returnable))
        history.record(history.ALMANAC, "File types changed hands",
                       items=mimeguard.summary(moved))
        dialog.connect("response", respond)
        dialog.present(self)
        return False

    # ---- install from a link -----------------------------------------------
    def on_install_link(self, _button=None, link: str = ""):
        """Paste a GitHub link; see every way it ships before choosing one."""
        entry = Gtk.Entry(placeholder_text="https://github.com/owner/project",
                          activates_default=True, hexpand=True)
        entry.set_text(link)
        dialog = Adw.AlertDialog(
            heading="Install from a link",
            body="Paste a GitHub project link. Every way it is published is "
                 "compared, and one is recommended, before anything is "
                 "downloaded.")
        dialog.set_extra_child(entry)
        dialog.add_response("cancel", "Cancel")
        dialog.add_response("find", "Look it up")
        dialog.set_response_appearance("find", Adw.ResponseAppearance.SUGGESTED)
        dialog.set_default_response("find")
        dialog.set_close_response("cancel")

        def respond(_d, resp):
            if resp != "find":
                return
            text = entry.get_text().strip()
            if not sources.parse(text):
                self._say("That is not a GitHub repository link")
                return
            self._say("Looking it up…")

            def work():
                res = sources.resolve(text)
                GLib.idle_add(self._show_link_options, res)
            threading.Thread(target=work, daemon=True).start()
        dialog.connect("response", respond)
        dialog.present(self)

    def _show_link_options(self, res):
        if not res.options:
            self._say(res.error or "Nothing installable found", 8)
            return False
        chosen = {"opt": res.best}
        rows = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
        rows.add_css_class("boxed-list")
        group = None
        for i, o in enumerate(res.options):
            # Name and version on the title; the file or package name, then
            # each reason and caveat on its own line, so none is cut off.
            lines = [o.name] + [f"✓ {r}" for r in o.reasons] \
                + [f"⚠ {w}" for w in o.warnings]
            row = self._plain(title=f"{o.title} · {o.version}" if o.version
                              else o.title, subtitle="\n".join(lines))
            row.set_subtitle_lines(0)
            check = Gtk.CheckButton(valign=Gtk.Align.CENTER)
            if group is None:
                group = check
            else:
                check.set_group(group)
            check.set_active(i == 0)
            check.connect("toggled", lambda b, o=o:
                          b.get_active() and chosen.update(opt=o))
            row.add_prefix(check)
            row.set_activatable_widget(check)
            if i == 0:
                pill = Gtk.Label(label="Recommended", valign=Gtk.Align.CENTER)
                pill.add_css_class("pill-ok")
                row.add_suffix(pill)
            rows.append(row)
        scroll = Gtk.ScrolledWindow()
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroll.set_propagate_natural_height(True)
        scroll.set_max_content_height(420)
        scroll.set_child(rows)

        facts = [f"{res.stars:,} stars"]
        if res.license:
            facts.append(res.license)
        if res.archived:
            facts.append("archived")
        dialog = Adw.AlertDialog(heading=f"Install {res.repo}?",
                                 body=(res.description + "\n" if res.description
                                       else "") + " · ".join(facts))
        dialog.set_body_use_markup(False)
        if hasattr(dialog, "set_prefer_wide_layout"):        # libadwaita 1.6+
            dialog.set_prefer_wide_layout(True)
        dialog.set_extra_child(scroll)
        dialog.add_response("cancel", "Cancel")
        dialog.add_response("install", "Install")
        dialog.set_response_appearance("install",
                                       Adw.ResponseAppearance.SUGGESTED)
        dialog.set_default_response("install")
        dialog.set_close_response("cancel")
        dialog.connect("response", lambda _d, r: r == "install"
                       and self._install_option(res, chosen["opt"]))
        dialog.present(self)
        return False

    def _install_option(self, res, opt):
        holder = self._watch_defaults()

        def after(ok):
            if ok:
                self._check_defaults(holder)
            self._after_pacman(ok)

        if opt.kind == sources.APPIMAGE:
            tasks.run(self, f"Installing {res.repo}",
                      [(f"Downloading {opt.name}",
                        lambda log: sources.install_appimage(res, opt, log))],
                      after)
            return
        argv = ["yay", "-S", "--needed", "--noconfirm", "--sudo", "pkexec",
                opt.name]
        name = opt.name
        tasks.run(self, f"Installing {name}",
                  self._snapshot_first([(f"Installing {name}", argv)],
                                       f"installing {name}"),
                  self._checked(verify.installed_package(name),
                                lambda b, a: verify.Outcome(
                                    bool(a) and a != b,
                                    f"{name} {a} installed" if a and a != b
                                    else f"{name} was not installed"),
                                then=after))

    def _after_pacman(self, ok: bool):
        """The app ran it, so it knows to re-read rather than guess."""
        if ok:
            self.reload()
        return False

    def _remove_appimage(self, app: inventory.App):
        """AppImages are ours, so remove them directly -- no package manager
        is involved and there is nothing to confirm against."""
        reg = inventory._load_registry()
        key = app.extra.get("key", app.name)
        path = Path(app.exec_path)
        try:
            path.unlink(missing_ok=True)
            path.with_suffix(".previous").unlink(missing_ok=True)
        except OSError as e:
            self._say(f"Could not delete {path.name}: {e}")
            return
        reg.pop(key, None)
        inventory.save_registry(reg)
        appimage_mod.remove_desktop_entry(key)
        self._say(f"Removed {app.name}")
        self.reload()

    def on_update_one(self, _button, app: inventory.App):
        """Update one thing, with whatever tool owns it."""
        if app.extra.get("update_cmd"):
            self._update_agent(app)
            return

        # A release only checkupdates knows about cannot be installed on its
        # own: the system database would need -y first, and syncing for one
        # package is the partial upgrade Arch warns against. Running yay
        # anyway does nothing and reports success, so say what will work.
        if (app.source != inventory.AUR
                and inventory.synced_version(app.name) not in ("", app.update)):
            tasks.confirm(
                self, f"{app.name} needs a full upgrade",
                f"{app.update} was released after this system's package "
                f"database was last refreshed, so it cannot be installed on "
                f"its own. “Update all” refreshes the database and installs "
                f"it along with everything it depends on.",
                "", "Update all", False,
                lambda: self.on_update_all(None))
            return

        # `yay -S <pkg>` without -y installs against the database as it
        # stands. That is a partial upgrade, which Arch warns against, so the
        # warning is in the dialog and Update all remains the safe default.
        argv = ["yay", "-S", "--needed", "--noconfirm", "--sudo", "pkexec", app.name]
        tasks.confirm(
            self, f"Update {app.name} only?",
            "Arch prefers full system upgrades. Updating one package against "
            "a database that has moved on is a partial upgrade, which can "
            "break linking. “Update all” is the safer choice.",
            f"{app.name}  {app.version} → {app.update}\n\n$ {' '.join(argv)}",
            "Update anyway", False,
            lambda: tasks.run(
                self, f"Updating {app.name}",
                self._snapshot_first([(f"Updating {app.name}", argv)],
                                     f"updating {app.name}"),
                self._checked(verify.for_app(app),
                              lambda b, a: verify.judge_app(app, b, a),
                              then=self._after_pacman)))

    def _update_agent(self, app: inventory.App):
        """Agents update themselves, or their version manager does it."""
        cmd = app.extra.get("update_cmd") or []
        if not cmd:
            self._say(
                f"{app.name} has no self-update command — "
                f"{app.version} is installed, {app.update} is out")
            if app.extra.get("url"):
                self.open_url(app.extra["url"])
            return
        if agents_mod.major_jump(app.version, app.update):
            # A new major version is shown with what changed, and asked
            # about on its own: it can rename flags or move config.
            def work():
                notes = agents_mod.release_notes(app.extra.get("repo", ""))
                GLib.idle_add(self._confirm_agent, app, cmd, notes)
            threading.Thread(target=work, daemon=True).start()
            return
        self._confirm_agent(app, cmd, "")

    def _confirm_agent(self, app, cmd, notes: str):
        manager = app.extra.get("manager", "self")
        major = agents_mod.major_jump(app.version, app.update)
        heading = (f"{app.name} {app.update} is a major release" if major
                   else f"Update {app.name}?")
        body = (f"Major versions can change commands, flags and config "
                f"formats. Read what changed first. Managed by {manager}; "
                f"nothing here needs root." if major else
                f"Managed by {manager}. It replaces its own binary, so nothing "
                f"here needs root.")
        detail = f"{app.name}  {app.version} → {app.update}\n\n$ {' '.join(cmd)}"
        if notes:
            detail += "\n\nWhat changed\n" + notes
        tasks.confirm(
            self, heading, body, detail,
            "Update" if not major else f"Update to {app.update}", False,
            lambda: tasks.run(self, f"Updating {app.name}",
                              [(f"Updating {app.name}", cmd)],
                              self._checked(verify.for_app(app),
                                            lambda b, a: verify.judge_app(app, b, a),
                                            then=self._after_pacman)))
        return False

    def on_git_pull(self, _button, app: inventory.App):
        """Update a local checkout, taking any local edits into account.

        A clean checkout is a fast-forward. One with edits is first held up
        against the update (patches.py): an edit the update already contains
        is dropped, one that still applies is carried over, and one that
        conflicts is set aside as a stash. Edits are never discarded.
        """
        repo = app.extra.get("repo", "")
        if not repo:
            return

        def work():
            state = patches.analyze(repo)
            GLib.idle_add(self._confirm_pull, app, repo, state)
        threading.Thread(target=work, daemon=True).start()

    def _confirm_pull(self, app, repo, state):
        steps = patches.steps(repo, state, app.name)
        done = self._checked(verify.git_head(repo),
                             lambda b, a: verify.judge_app(app, b, a),
                             then=self._after_pacman)
        if state.kind in (patches.CLEAN, patches.UNKNOWN):
            tasks.run(self, f"Updating {app.name}", steps, done)
            return False
        heading, body, action = patches.explain(state, app.name)
        detail = "\n".join(f"$ {' '.join(argv)}" for _l, argv in steps)
        if state.upstream_commits:
            detail += ("\n\nUpstream changes to the same files\n"
                       + "\n".join(state.upstream_commits[:15]))
        tasks.confirm(self, heading, body, detail, action, False,
                      lambda: tasks.run(self, f"Updating {app.name}", steps, done))
        return False

    def on_update_appimage(self, button, app: inventory.App):
        """Download the newer AppImage and swap it in, off the UI thread."""
        url = app.extra.get("download", "")
        if not url:
            self._say(f"No download URL for {app.name}")
            return
        dest = Path(app.exec_path)
        button.set_sensitive(False)
        self._summarize(f"Downloading {app.name} {app.update}…")

        def work():
            try:
                appimage_mod.download(url, dest)
                reg = inventory._load_registry()
                key = app.extra.get("key", app.name)
                if key in reg:
                    reg[key]["version"] = app.update
                    # Force a re-check rather than trusting the stale entry.
                    reg[key]["checked_at"] = 0
                    reg[key]["latest"] = {}
                    inventory.save_registry(reg)
                appimage_mod.install_desktop_entry(dest, key)
                GLib.idle_add(self._after_appimage, app.name, app.update, None)
            except Exception as e:                      # network, disk, perms
                GLib.idle_add(self._after_appimage, app.name, app.update, e)

        threading.Thread(target=work, daemon=True).start()

    def _after_appimage(self, name, version, error):
        if error:
            self._say(f"{name} update failed: {error}")
        else:
            self._say(f"{name} updated to {version}")
            self.reload()
        return False

    def on_update_all(self, _button):
        """Hand the full upgrade to the system's own updater, in a terminal.

        The terminal is the point: `omarchy update` needs a password, and a GUI
        cannot answer that prompt. Running it where the user can type is the
        honest solution; the alternative is passwordless root.

        The list rescans itself when the terminal closes. Leaving that to the
        user meant everything just updated stayed listed as needing an update.
        """
        command = xdg.system_update_command()
        argv = xdg.terminal_argv(
            ["bash", "-lc",
             f"{command}; echo; read -n1 -r -p 'Done — press any key to close'"])
        if not argv:
            self._say("No terminal found to run the update in")
            return
        try:
            started = time.monotonic()
            proc = subprocess.Popen(argv, start_new_session=True)
        except OSError as e:
            self._say(f"Could not start the update: {e}")
            return
        self._say("Update running in a terminal — the list refreshes when it closes")
        probe = verify.pending_updates()

        def wait():
            # Asked while the user types their password, which is longer
            # than checkupdates takes.
            before = probe()
            proc.wait()
            GLib.idle_add(self._after_pacman, True)
            if time.monotonic() - started < 5:
                # The launcher handed the window to a terminal server and
                # returned at once, so there is no "finished" to wait for.
                GLib.idle_add(self._say, "Rescan when the update finishes", 8)
                return
            GLib.idle_add(self._report_outcome,
                          verify.judge_pending(before, probe()))

        threading.Thread(target=wait, daemon=True).start()

    def on_add_appimage(self, _button):
        dialog = Gtk.FileDialog(title="Choose an AppImage to track")
        dialog.open(self, None, self._on_appimage_chosen)

    def _on_appimage_chosen(self, dialog, result):
        try:
            gfile = dialog.open_finish(result)
        except GLib.Error:
            return                      # cancelled
        if gfile:
            self._install_appimage(Path(gfile.get_path()))

    def _install_appimage(self, src: Path):
        """Copy into our own store and register it.

        Copying rather than referencing the download means a later cleanup of
        ~/Downloads cannot quietly break an installed app.
        """
        store = Path(inventory.REGISTRY).parent / "apps"
        store.mkdir(parents=True, exist_ok=True)
        name = src.stem.split("-")[0] or src.stem
        dest = store / f"{name}.AppImage"
        try:
            shutil.copy2(src, dest)
            dest.chmod(dest.stat().st_mode | 0o111)
        except OSError as e:
            self._say(f"Could not install: {e}")
            return

        version = "?"
        try:
            out = subprocess.run([str(dest), "--version"], capture_output=True,
                                 text=True, timeout=12).stdout.strip()
            if out and len(out) < 60:
                version = out.splitlines()[0]
        except (OSError, subprocess.SubprocessError):
            pass                        # plenty of AppImages ignore --version

        reg = inventory._load_registry()
        reg[name] = {"name": name, "path": str(dest), "version": version,
                     "description": f"AppImage · {src.name}", "source_file": str(src)}
        inventory.save_registry(reg)
        # A tracked AppImage should appear in the launcher like anything else,
        # not just inside this window.
        appimage_mod.install_desktop_entry(dest, name)
        self._say(f"Tracking {name}")
        self.reload()


class AlmanacApp(Adw.Application):
    def __init__(self):
        super().__init__(application_id="io.github.twodxme.Almanac",
                         flags=Gio.ApplicationFlags.HANDLES_COMMAND_LINE)
        self.start_page = ""
        self.add_main_option(
            "page", ord("p"), GLib.OptionFlags.NONE, GLib.OptionArg.STRING,
            "Open on a specific page: store, updates, ai, health, apps, all",
            "PAGE")

    # Opening straight to a page makes Almanac scriptable -- a bar widget or
    # a keybinding can land on Updates without a click -- and makes a
    # screenshot of a given page reproducible.
    PAGE_ALIASES = {
        "featured": "featured", "home": "featured",
        "store": "discover", "app-store": "discover", "discover": "discover",
        "themes": "themes", "theme": "themes",
        "updates": "updates", "update": "updates",
        "ai": inventory.AGENT, "agents": inventory.AGENT,
        "health": "maintenance", "maintenance": "maintenance",
        "history": "history", "timeline": "history",
        "apps": "apps", "applications": "apps",
        "all": "all", "everything": "all",
        "orphans": inventory.ORPHAN,
    }

    def do_command_line(self, command_line):
        opts = command_line.get_options_dict().end().unpack()
        wanted = (opts.get("page") or "").strip().lower()
        if wanted:
            resolved = self.PAGE_ALIASES.get(wanted)
            if resolved is None:
                command_line.printerr_literal(
                    f"almanac: unknown page {wanted!r}. Try one of: "
                    + ", ".join(sorted(set(self.PAGE_ALIASES))) + "\n")
                return 2
            self.start_page = resolved
        self.activate()
        return 0

    def do_startup(self):
        Adw.Application.do_startup(self)
        # Almanac's own glyphs, shipped beside the code so they look the same
        # under any icon theme.
        display = Gdk.Display.get_default()
        if display is not None:
            Gtk.IconTheme.get_for_display(display).add_search_path(
                str(Path(__file__).resolve().parent / "icons"))

    def do_activate(self):
        win = self.props.active_window
        if win is None:
            win = AlmanacWindow(application=self)
            win.connect("close-request", self._on_close)
        if self.start_page:
            # After the restored category and the first inventory pass, so a
            # late callback cannot pull the window off the requested page.
            GLib.idle_add(win._select_category, self.start_page,
                          priority=GLib.PRIORITY_LOW)
        win.present()

    @staticmethod
    def _on_close(win):
        win._save_state()
        return False                    # let the close proceed


if __name__ == "__main__":
    sys.exit(AlmanacApp().run(sys.argv))
