//! UI section `shell` (port of `app.py` lines 1–1454 and 6481–end).
//!
//! almanac — one window for everything installed on this machine.
//!
//! Design rule: this app owns the VIEW and the AppImage registry. It does not
//! own package state: installs and updates are handed to the tool that owns
//! them (yay, pacman, mise, git), with root requested through polkit, and a
//! full upgrade runs in a real terminal through the system's own updater.
//! Every result is then checked rather than assumed -- see verify.py.
//!
//! This module holds the helpers, the stylesheet, the list items, the window
//! itself (`Win` / `Inner` / `Shared`), the sidebar, header and columns,
//! persisted window state, external change detection, `AlmanacApp` and the
//! CLI, one file each under `shell/`; everything is re-exported here, so
//! paths stay `shell::name`. Every other section adds its own `impl Win`
//! block (ARCHITECTURE.md).

mod app;
mod build;
mod columns;
mod guard;
mod helpers;
mod items;
mod shape;
mod state;
mod tables;
mod watch;
mod window;

pub use app::{run, AlmanacApp, PAGE_ALIASES, application_id, icons_dir, resolve_page};
pub(in crate::ui) use helpers::as_text;
pub use helpers::{
    age, compact, exc_str, fallback_icon, fmt_size, lower_cmp, name, primary_category, reverse_text,
    source_label, spawn_bg, spawn_bg_checked, on_bg_failure, panic_text, thumb_bg, version_text, SwitchGuard, CSS, FALLBACK_ICON, INSTALL_ICON,
    SOURCE_LABEL,
};
pub use items::{AppItem, PluginItem};
pub use shape::{page_body, section_note, severity_pill, PAGE_MARGIN, PAGE_MARGIN_BOTTOM, PAGE_SPACING};
pub use tables::{
    page_accel, page_spec, sorts_discover, sorts_installed, GroupItem, PageSpec, COLLAPSED_BY_DEFAULT,
    GROUPS, PAGES, PAGE_KEYS,
};
pub use window::{tex_bytes, Findings, GroupHeader, Inner, Shared, SidebarRow, State, TexCache, WeakWin, Win, TEX_BUDGET};

/// `self._primary`: (label, enabled, tooltip, handler) of the current page's
/// main action. One type for both sections: the packages section defines it,
/// since `_primary_actions` builds it.
pub use crate::ui::packages::PrimaryAction;
