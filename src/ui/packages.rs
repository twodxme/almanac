//! UI section `packages` (port of `app.py` lines 1454-2481).
//!
//! Data loading and filtering: the Updates, Applications, Everything and
//! Orphans tables, the detail dialog, the sidebar's selection and counts,
//! page chrome, the header's primary action, theming and the status line.
//! One file per concern under `packages/`, re-exported here.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::inventory;
use crate::ui::shell::Win;

mod chrome;
mod counts;
mod decisions;
mod details;
mod extra;
mod filtering;
mod loading;

pub use decisions::{
    available_label, detail_badges_for, group_state, match_item, sort_cmp, GroupState, SORT_KEYS,
};
pub use extra::{py_strip, thousands, DetailValue};

/// The header's primary action for one page: (label, enabled, tooltip,
/// handler), as `_primary_actions` returns it.
#[derive(Clone)]
pub struct PrimaryAction {
    pub label: String,
    pub enabled: bool,
    pub tooltip: String,
    pub handler: Rc<dyn Fn(&Win)>,
}

/// Rows `plain` can build: the Python passed the row class (`cls`), which
/// is `Adw.ActionRow` by default and `Adw.ExpanderRow` in a few places.
pub trait PlainRow: IsA<adw::PreferencesRow> {
    fn new_plain() -> Self;
    fn put_subtitle(&self, subtitle: &str);
}

impl PlainRow for adw::ActionRow {
    fn new_plain() -> Self {
        adw::ActionRow::new()
    }
    fn put_subtitle(&self, subtitle: &str) {
        self.set_subtitle(subtitle);
    }
}

impl PlainRow for adw::ExpanderRow {
    fn new_plain() -> Self {
        adw::ExpanderRow::new()
    }
    fn put_subtitle(&self, subtitle: &str) {
        self.set_subtitle(subtitle);
    }
}

/// State only this section uses. Everything else this section reads or
/// writes (`_discovering`, `_discover_timer`, `_narrow`, `_primary`,
/// `_reported`, the `*_loaded_once` flags, `_manual_sorter`) is read by other
/// sections too and lives in `shell::Shared`.
#[derive(Default)]
pub struct State {
    /// Coalesces theme-change bursts into one restyle (`_theme_timer`).
    pub theme_timer: Option<glib::SourceId>,
    /// A scan (`reload`) is running. One runs at a time: every action, the
    /// pacman.log watcher and page changes each asked for their own, and
    /// they piled up.
    pub scanning: bool,
    /// The search box's pending filter pass: typing re-filters once it
    /// pauses rather than on every key.
    pub search_timer: Option<glib::SourceId>,
    /// `reload` was called while a scan ran: that scan's answer predates
    /// whatever prompted the call, so it is dropped and one more scan runs.
    pub scan_again: bool,
    /// When the running or last scan started; the pacman.log watcher has
    /// nothing to add when the log has not changed since.
    pub scan_started: Option<std::time::SystemTime>,
    /// What the last scan found, to put back when the App Store hands the
    /// list back (it shows its search results in the same list).
    pub last_scan: Option<Vec<inventory::App>>,
}

/// `_say`'s default timeout, in seconds. Rust has no default arguments, so
/// callers that relied on it pass this.
pub const SAY_TIMEOUT: u32 = 5;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    /// This section's files: this one and every file under packages/.
    const SRC: &[&str] = &[
        include_str!("packages.rs"),
        include_str!("packages/extra.rs"),
        include_str!("packages/decisions.rs"),
        include_str!("packages/loading.rs"),
        include_str!("packages/details.rs"),
        include_str!("packages/counts.rs"),
        include_str!("packages/filtering.rs"),
        include_str!("packages/chrome.rs"),
    ];

    #[test]
    fn only_the_page_on_show_labels_the_header_button() {
        // A scan finishing under Health once relabelled its button "Update
        // all (N)" while the button still ran Reclaim space.
        let writers: usize = SRC
            .iter()
            .map(|s| s.matches(concat!("update_btn", ".set_label(")).count())
            .sum();
        assert_eq!(writers, 1, "sync_primary is the one place the label is set");
    }
    const CSS: &str = include_str!("../../data/style.css");

    /// The code part of this section's files, without their tests.
    fn code() -> &'static str {
        static CODE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            SRC.iter()
                .map(|s| s.split("#[cfg(test)]").next().unwrap_or(s))
                .collect::<Vec<_>>()
                .join("\n")
        });
        &CODE
    }

    /// test_a_folded_groups_selected_row_folds_once_left: drives
    /// `_apply_all_group_states` over group_rows and group_members, as the
    /// Python does with mocks, and records each set_visible.
    #[test]
    fn a_folded_groups_selected_row_folds_once_left() {
        #[derive(Clone, Debug, PartialEq)]
        struct Row(&'static str);
        let group_rows = vec![
            ("primary".to_string(), None::<()>),
            ("library".to_string(), Some(())),
        ];
        let mut group_members = BTreeMap::new();
        group_members.insert("library".to_string(), vec![Row("apps"), Row("plugins")]);
        let collapsed: BTreeSet<String> = ["library".to_string()].into_iter().collect();
        // _apply_all_group_states: every group in turn; returns the last
        // set_visible each row got.
        let apply_all = |filter_key: &str| {
            let mut seen: BTreeMap<&'static str, bool> = BTreeMap::new();
            for (gkey, _) in &group_rows {
                if let Some(plan) = group_state(
                    &group_rows,
                    &group_members,
                    &collapsed,
                    filter_key,
                    gkey,
                    |r| r.0,
                ) {
                    assert_eq!(plan.icon, "pan-end-symbolic");
                    for (row, visible) in plan.members {
                        seen.insert(row.0, visible);
                    }
                }
            }
            seen
        };
        let seen = apply_all("plugins");
        assert_eq!(seen.get("plugins"), Some(&true)); // selected: shown
        assert_eq!(seen.get("apps"), Some(&false));
        let seen = apply_all("updates"); // navigated away
        assert_eq!(seen.get("plugins"), Some(&false)); // now folded
                                                       // Ungrouped primaries never fold; an unfolded group shows everyone.
        assert!(group_state(
            &group_rows,
            &group_members,
            &collapsed,
            "x",
            "primary",
            |r| r.0
        )
        .is_none());
        let open = group_state(
            &group_rows,
            &group_members,
            &BTreeSet::new(),
            "x",
            "library",
            |r| r.0,
        )
        .map(|p| (p.icon, p.members.iter().all(|(_, v)| *v)));
        assert_eq!(open, Some(("pan-down-symbolic", true)));
        let body = code()
            .split("pub fn on_sidebar_selected")
            .nth(1)
            .and_then(|s| s.split("pub fn set_narrow").next())
            .unwrap_or("");
        assert!(body.contains("self.apply_all_group_states()"));
    }

    /// Package relations carry version constraints (`glibc>=2.40`) and
    /// project URLs carry query strings (`?a=1&b=2`): read as Pango markup,
    /// either one blanked its row. Every row in the details dialog goes
    /// through `Win::plain`, which switches markup off before the text
    /// goes in, and toasts show names and errors as written.
    #[test]
    fn details_and_toasts_show_outside_text_as_written() {
        let details = code()
            .split("fn build_details")
            .nth(1)
            .and_then(|s| s.split("let dialog = adw::Dialog::new()").next())
            .unwrap_or("");
        assert!(
            details.contains("\"Dependencies\""),
            "the split found the dialog"
        );
        assert!(
            !details.contains("ActionRow::builder()"),
            "a row built with markup on"
        );
        let toast = code()
            .split("fn toast(")
            .nth(1)
            .and_then(|s| s.split("\n    }").next())
            .unwrap_or("");
        assert!(toast.contains(".use_markup(false)"));
        let say = code()
            .split("pub fn say(")
            .nth(1)
            .and_then(|s| s.split("\n    }").next())
            .unwrap_or("");
        assert!(
            say.contains("self.toast("),
            "say shows its toast through toast"
        );
        // A notification's text is put in plain words once: its fallback to
        // the status line is `toast`, not `say`, which would do it again.
        let notify = code()
            .split("pub fn try_notify(")
            .nth(1)
            .and_then(|s| s.split("\n    }").next())
            .unwrap_or("");
        assert!(
            notify.contains("human_error(title)") && notify.contains("self.toast("),
            "the split found try_notify"
        );
        assert!(!notify.contains("self.say("));

        // A package build's check() has no display: adw::init can still
        // answer Ok there, and building a row then crashes the test binary.
        // Looking first keeps adw::init's NULL-display criticals out of the
        // build log; CI's Broadway sets GDK_BACKEND and neither variable.
        let set = |var| std::env::var_os(var).is_some_and(|v| !v.is_empty());
        let broadway = std::env::var("GDK_BACKEND").is_ok_and(|b| b.contains("broadway"));
        if !(set("WAYLAND_DISPLAY") || set("DISPLAY") || broadway) {
            return;
        }
        if !matches!(std::panic::catch_unwind(adw::init), Ok(Ok(())))
            || gtk::gdk::Display::default().is_none()
        {
            return; // no display, or GTK already belongs to another test thread
        }
        let text = "glibc>=2.40 lib<foo> a&b";
        let row: adw::ActionRow = Win::plain("Requires", text);
        assert!(!row.uses_markup());
        assert_eq!(row.subtitle().as_deref(), Some(text));
    }

    /// test_every_icon_only_button_is_named, against this file.
    #[test]
    fn every_icon_only_button_is_named() {
        let lines: Vec<&str> = code().lines().collect();
        let made = regex::Regex::new(
            r"let (\w+) = gtk::(?:Menu|Toggle)?Button::(?:builder\(\)|from_icon_name\()",
        )
        .unwrap();
        let mut unnamed = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(m) = made.captures(line) else {
                continue;
            };
            let var = &m[1];
            let window = lines[i..(i + 12).min(lines.len())].join("\n");
            let is_icon = window.lines().take(3).any(|l| l.contains("icon_name"))
                || line.contains("from_icon_name");
            if !is_icon {
                continue;
            }
            if !window.contains(&format!("name(&{var}")) && !window.contains("Property::Label") {
                unnamed.push(format!("packages.rs:{} {var}", i + 1));
            }
        }
        assert_eq!(unnamed, Vec::<String>::new());
    }

    /// test_spacing_comes_from_the_scale, against this file.
    #[test]
    fn spacing_comes_from_the_scale() {
        let scale: BTreeSet<i64> = [2, 4, 6, 8, 12, 16, 20, 24].into_iter().collect();
        let prop = regex::Regex::new(
            r"\.(margin_(?:start|end|top|bottom)|spacing|child_spacing|line_spacing)\((\d+)\)",
        )
        .unwrap();
        let boxed = regex::Regex::new(r"Box::new\(gtk::Orientation::\w+, (\d+)\)").unwrap();
        let mut offenders = Vec::new();
        for c in prop.captures_iter(code()) {
            if !scale.contains(&c[2].parse::<i64>().unwrap()) {
                offenders.push(format!("{}={}", &c[1], &c[2]));
            }
        }
        for c in boxed.captures_iter(code()) {
            if !scale.contains(&c[1].parse::<i64>().unwrap()) {
                offenders.push(format!("spacing={}", &c[1]));
            }
        }
        assert_eq!(offenders, Vec::<String>::new());
    }

    /// test_badges_use_one_pill_system, against this file.
    #[test]
    fn badges_use_one_pill_system() {
        let retired = [
            "crit-pill",
            "stale-pill",
            "crit-core",
            "store-chip",
            "store-installed",
            "trust-official",
            "trust-ok",
            "trust-warn",
            "trust-none",
        ];
        let found: Vec<&str> = retired
            .iter()
            .copied()
            .filter(|n| code().contains(&format!("\"{n}\"")))
            .collect();
        assert_eq!(found, Vec::<&str>::new());
    }

    /// test_every_pill_class_is_defined_in_the_stylesheet, against this file.
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        let pills: BTreeSet<&str> = [
            "pill",
            "pill-crit",
            "pill-warn",
            "pill-ok",
            "pill-accent",
            "pill-quiet",
        ]
        .into_iter()
        .collect();
        let used: BTreeSet<String> = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#)
            .unwrap()
            .captures_iter(code())
            .map(|c| c[1].to_string())
            .collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for n in &used {
            assert!(
                CSS.contains(&format!(".{n}")),
                "{n} is applied but never styled"
            );
            assert!(pills.contains(n.as_str()), "unknown pill variant {n}");
        }
        // The sidebar count's levels are styled too.
        for cls in [
            "count-badge.is-warn",
            "count-badge.is-crit",
            "stat-key",
            "stat-val",
        ] {
            assert!(
                CSS.contains(&format!(".{cls}")),
                "{cls} is applied but never styled"
            );
        }
    }

    /// test_pills_are_all_one_size
    #[test]
    fn pills_are_all_one_size() {
        let block = regex::Regex::new(r"\.pill,[^{]*\{([^}]*)\}").unwrap();
        let shared = block
            .captures(CSS)
            .expect("the shared pill rule is missing");
        assert!(
            shared[1].contains("font-size"),
            "the pill size must be set once"
        );
        for variant in [
            "pill-crit",
            "pill-warn",
            "pill-ok",
            "pill-accent",
            "pill-quiet",
        ] {
            let rule = regex::Regex::new(&format!(r"(?m)^\.{variant}\s*\{{([^}}]*)\}}")).unwrap();
            if let Some(r) = rule.captures(CSS) {
                assert!(
                    !r[1].contains("font-size"),
                    ".{variant} must not set its own size"
                );
            }
        }
    }

    fn stems(dir: &Path, out: &mut BTreeSet<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stems(&p, out);
            } else if matches!(p.extension().and_then(|x| x.to_str()), Some("svg" | "png")) {
                if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
                    out.insert(s.to_string());
                }
            }
        }
    }

    /// test_every_icon_name_is_in_adwaita_or_shipped_here, for this file.
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return; // Adwaita icon theme not installed
        }
        let mut have = BTreeSet::new();
        stems(adwaita, &mut have);
        stems(
            &PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/icons")),
            &mut have,
        );
        let names: BTreeSet<String> = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#)
            .unwrap()
            .captures_iter(code())
            .map(|c| c[1].to_string())
            .collect();
        // adw-* icons ship inside libadwaita itself.
        let missing: Vec<&String> = names
            .iter()
            .filter(|n| !have.contains(*n) && !n.starts_with("adw-"))
            .collect();
        assert_eq!(missing, Vec::<&String>::new());
    }
}
