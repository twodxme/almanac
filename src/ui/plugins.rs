//! UI section `plugins` (port of `app.py` lines 3695-3993 and 5636-5999):
//! the Plugins page; on/off, shortcuts, bulk updates, the shell — and the
//! row actions, install and remove that sit beside them in `app.py`. One
//! file per concern under `plugins/`, re-exported here.

use crate::plugins as plugins_mod;

mod controls;
mod page;
mod pyisms;
mod removal;
mod updates;

pub use page::{plugin_matches, plugin_menu, plugin_state_key, plugin_state_pill, switch_name, PLUGIN_STATE_PILLS};
pub use removal::{plan_orphan_cleanup, plan_package_removal, split_orphans, RemovalPlan};
pub(in crate::ui) use updates::{plugin_step, plugin_update_states};
pub use updates::{plan_plugin_updates, UpdatePlan};

/// State only the Plugins section reads and writes.
/// What `plugin_logo` shows beside a plugin (see `plugin_logo_choice`).
/// The system icon shows first in every case; the others replace it when
/// their image arrives.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LogoChoice {
    Vendor { key: String, domain: String },
    Art { thumb: String },
    Avatar { key: String, url: String },
    Symbol { name: String },
}


#[derive(Default)]
pub struct State {
    /// `self.plugins_body`: the page's column of sections.
    pub plugins_body: Option<gtk::Box>,
    /// `self._plugin_rows`: every row shown, with the plugin it stands for
    /// (searched by `plugins_filter`, looked up by `plugin_action`).
    /// Each row's plugin is the one object its switch updates, as in the
    /// Python.
    pub plugin_rows: Vec<(adw::ActionRow, std::rc::Rc<std::cell::RefCell<plugins_mod::Installed>>)>,
    // `self._plugin_updates` lives in `shell::Shared::plugin_updates`: the
    // header's primary action reads it too.
}

#[cfg(test)]
mod tests {
    
    use std::path::Path;

    /// This section's files: this one and every file under plugins/.
    fn src() -> &'static str {
        static SRC: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            [
                include_str!("plugins.rs"),
                include_str!("plugins/page.rs"),
                include_str!("plugins/updates.rs"),
                include_str!("plugins/controls.rs"),
                include_str!("plugins/removal.rs"),
                include_str!("plugins/pyisms.rs"),
            ]
            .join("\n")
        });
        &SRC
    }

    fn css() -> &'static str {
        include_str!("../../data/style.css")
    }

    // Accessibility.test_every_icon_only_button_is_named, for this file.
    #[test]
    fn every_icon_only_button_is_named() {
        let lines: Vec<&str> = src().lines().collect();
        let made = regex::Regex::new(r"let (\w+) = gtk::(?:Menu|Toggle)?Button::builder\(\)").unwrap();
        let mut unnamed = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(m) = made.captures(line) else { continue };
            let var = &m[1];
            let window = lines[i..(i + 12).min(lines.len())].join("\n");
            if !window.contains(".icon_name(") {
                continue;
            }
            if !window.contains(&format!("name(&{var}")) && !window.contains("Property::Label") {
                unnamed.push(format!("plugins.rs:{} {var}", i + 1));
            }
        }
        assert_eq!(unnamed, Vec::<String>::new());
    }

    // VisualSystem.test_spacing_comes_from_the_scale, for this file.
    #[test]
    fn spacing_comes_from_the_scale() {
        const SCALE: [i64; 8] = [2, 4, 6, 8, 12, 16, 20, 24];
        let re = regex::Regex::new(
            r"\.(margin_(?:start|end|top|bottom)|spacing|child_spacing|line_spacing)\((\d+)\)",
        )
        .unwrap();
        let boxed = regex::Regex::new(r"gtk::Box::new\(gtk::Orientation::\w+, (\d+)\)").unwrap();
        let mut offenders = Vec::new();
        for c in re.captures_iter(src()) {
            let v: i64 = c[2].parse().unwrap();
            if !SCALE.contains(&v) {
                offenders.push(format!("{}={v}", &c[1]));
            }
        }
        for c in boxed.captures_iter(src()) {
            let v: i64 = c[1].parse().unwrap();
            if v != 0 && !SCALE.contains(&v) {
                offenders.push(format!("spacing={v}"));
            }
        }
        assert_eq!(offenders, Vec::<String>::new());
    }

    // VisualSystem.test_every_pill_class_is_defined_in_the_stylesheet
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        const PILLS: [&str; 6] = ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"];
        let used: std::collections::BTreeSet<String> = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#)
            .unwrap()
            .captures_iter(src())
            .map(|c| c[1].to_string())
            .collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for name in &used {
            assert!(css().contains(&format!(".{name}")), "{name} is applied but never styled");
            assert!(PILLS.contains(&name.as_str()), "unknown pill variant: {name}");
        }
    }

    #[test]
    fn plugin_logo_classes_are_styled() {
        assert!(css().contains(".plugin-logo"));
        assert!(css().contains(".plugin-logo.has-art"));
    }

    // IconsExist.test_every_icon_name_is_in_adwaita_or_shipped_here
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return; // Adwaita icon theme not installed
        }
        fn walk(d: &Path, out: &mut std::collections::BTreeSet<String>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if matches!(p.extension().and_then(|x| x.to_str()), Some("svg" | "png")) {
                    if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
                        out.insert(s.to_string());
                    }
                }
            }
        }
        let mut have = std::collections::BTreeSet::new();
        walk(adwaita, &mut have);
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("icons"), &mut have);
        let names: std::collections::BTreeSet<String> = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#)
            .unwrap()
            .captures_iter(src())
            .map(|c| c[1].to_string())
            .collect();
        let missing: Vec<&String> =
            names.iter().filter(|n| !have.contains(*n) && !n.starts_with("adw-")).collect();
        assert_eq!(missing, Vec::<&String>::new());
    }
}
