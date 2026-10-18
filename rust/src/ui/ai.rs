//! UI section `ai` (port of `app.py` lines 4486-4876): the AI page, plus the
//! shared page-shape helpers that live among those lines (`status_page`,
//! `labelled`, `section`) and the icon helpers used by every list.
//!
//! STUBS: signatures and state are final; bodies are placeholders that do
//! nothing harmful.

#![allow(dead_code, unused_variables)]

use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::glib;

use crate::ai::{Entry, Group};
use crate::inventory::App;
use crate::ui::shell::Win;

/// State only the AI page uses.
#[derive(Default)]
pub struct State {
    /// `self.ai_page`: the column the groups are rendered into.
    pub ai_page: Option<gtk::Box>,
    /// `self._ai_pending`: groups still loading. Signed because the Python
    /// tests `<= 0` after decrementing.
    pub ai_pending: i64,
}

/// What `section()` returns: the Python stashed the list on the box as
/// `box._rows`; here both halves are returned together.
#[derive(Clone)]
pub struct Section {
    pub root: gtk::Box,
    pub rows: gtk::ListBox,
}

/// `_AI_SEVERITY`: severity -> (pill label, pill css class).
pub const AI_SEVERITY: &[(&str, (&str, &str))] = &[
    ("warn", ("Attention", "pill-warn")),
    ("info", ("Optional", "pill")),
    ("ok", ("", "")),
];

/// `_AI_SEVERITY.get(sev, ("", ""))`.
pub fn ai_severity(severity: &str) -> (&'static str, &'static str) {
    AI_SEVERITY
        .iter()
        .find(|(k, _)| *k == severity)
        .map(|(_, v)| *v)
        .unwrap_or(("", ""))
}

/// Sort rank used by `ai_show`: trouble first.
pub fn severity_order(severity: &str) -> u8 {
    match severity {
        "warn" => 0,
        "info" => 1,
        _ => 2,
    }
}

fn extra_str(app: &App, key: &str) -> String {
    app.extra
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
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
        gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()
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
        adw::StatusPage::new()
    }

    /// A control with its name above it, for the filter drawer.
    pub fn labelled(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
        gtk::Box::new(gtk::Orientation::Vertical, 4)
    }

    pub fn store_reset_filters(&self) {}

    /// One titled, full-width list. The shared shape for secondary pages.
    pub fn section(title: &str, description: &str, trailing: Option<&gtk::Widget>) -> Section {
        Section {
            root: gtk::Box::new(gtk::Orientation::Vertical, 4),
            rows: gtk::ListBox::new(),
        }
    }

    /// Render each group as it arrives rather than waiting for all six.
    ///
    /// Checking MCP health means opening a connection to every configured
    /// server, which takes ten to twenty seconds on a cold cache. The skills
    /// and hooks are read from disk in milliseconds and should not be held
    /// behind it.
    pub fn ai_reload(&self) {}

    pub fn ai_group_done(&self, index: usize, title: &str, entries: Vec<Entry>) -> glib::ControlFlow {
        glib::ControlFlow::Break
    }

    // Keep what was rendered: revisiting the page re-renders from this
    // rather than reloading, and an unrecorded result showed as empty.
    pub fn ai_show(&self, groups: &[Group], final_: bool) -> glib::ControlFlow {
        glib::ControlFlow::Break
    }

    pub fn ai_row(&self, e: &Entry) -> adw::ActionRow {
        adw::ActionRow::new()
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
        glib::ControlFlow::Break
    }

    pub fn vendor_icon(&self, image: &gtk::Image, e: &Entry) {}

    /// `_set_icon(image, path, size=48)`.
    pub fn set_icon(&self, image: &gtk::Image, path: &Path, size: i32) {}

    pub fn ai_act(&self, e: &Entry) {}

    pub fn ai_after(&self, ok: bool) -> glib::ControlFlow {
        glib::ControlFlow::Break
    }

    pub fn ai_update_all(&self) {}
}
