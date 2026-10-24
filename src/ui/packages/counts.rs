//! Sidebar badges, the stats block and the sidebar's counts.

use std::collections::BTreeMap;

use adw::prelude::*;
use serde_json::Value;

use crate::inventory;
use crate::ui::shell::{AppItem, Win};

use super::extra::{py_space, py_type_name, truthy};

impl Win {
    /// Set a sidebar count and how loudly it should read.
    ///
    /// Three levels, and only one of them is red. Every badge used to turn
    /// red as soon as any single package anywhere was security-critical,
    /// which painted Featured, Updates and AI the same alarming colour and
    /// left the actual security item indistinguishable from the rest.
    pub fn badge(label: &gtk::Label, count: usize, level: &str) {
        label.set_text(&if count > 0 {
            count.to_string()
        } else {
            String::new()
        });
        for css in ["is-warn", "is-crit"] {
            label.remove_css_class(css);
        }
        label.set_visible(count > 0);
        if count == 0 {
            return;
        }
        if level == "crit" {
            label.add_css_class("is-crit");
        } else if level == "warn" {
            label.add_css_class("is-warn");
        }
    }

    pub fn stat_row(&self, label: &str, value: &str, tooltip: &str) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let name = gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .hexpand(true)
            .build();
        name.add_css_class("stat-key");
        let val = gtk::Label::builder().label(value).xalign(1.0).build();
        val.add_css_class("stat-val");
        row.append(&name);
        row.append(&val);
        if !tooltip.is_empty() {
            row.set_tooltip_text(Some(tooltip));
        }
        row
    }

    /// The numbers you would otherwise open a terminal to get.
    ///
    /// Err where Python's raised: a non-string `size` (size_mb calls
    /// `.split()` on it) or an unreadable uptime. The grid is left as it was
    /// at that point -- emptied, nothing attached.
    pub fn refresh_stats(&self) -> Result<(), String> {
        while let Some(child) = self.stats.first_child() {
            self.stats.remove(&child);
        }

        let items = self.store_objects();
        let total = Win::count_items(&items, |i| i.source != inventory::ORPHAN);
        let mut size = 0.0_f64;
        for o in &items {
            let Some(i) = AppItem::of(o) else { continue };
            // extra.get("size", "") goes to size_mb as it is.
            size += match i.app.extra.get("size") {
                None => inventory::size_mb(""),
                Some(Value::String(text)) => inventory::size_mb(text),
                Some(other) => {
                    return Err(format!(
                        "AttributeError: '{}' object has no attribute 'split'",
                        py_type_name(other)
                    ))
                }
            };
        }
        let explicit =
            Win::count_items(&items, |i| i.source == inventory::REPO || i.source == inventory::AUR);
        let foreign = Win::count_items(&items, |i| i.source == inventory::AUR);
        // Counting packages, not releases: summing release deltas across
        // date-versioned and epoch-versioned packages produced numbers in the
        // tens of thousands, which described nothing.
        let majors = Win::count_items(&items, |i| i.behind != 0 && i.behind_kind == "major");
        let pending = Win::count_items(&items, |i| i.has_update);
        let security = Win::count_items(&items, |i| i.critical == "security");

        let mut rows: Vec<(String, String, String)> = vec![
            (
                "Installed".into(),
                format!("{total}"),
                "Everything Almanac can see, orphans aside".into(),
            ),
            (
                "Disk".into(),
                crate::maintenance::fmt_size(size),
                "Installed size reported by the package manager".into(),
            ),
            (
                "Updates".into(),
                format!("{pending}"),
                "Packages, AppImages, plugins and agents with a newer version".into(),
            ),
            (
                "AUR".into(),
                format!("{foreign}"),
                format!("{foreign} of {explicit} repo and AUR packages are foreign"),
            ),
        ];
        if majors != 0 {
            rows.push((
                "Majors".into(),
                format!("{majors}"),
                "Packages at least one major version behind upstream".into(),
            ));
        }
        if security != 0 {
            rows.push((
                "Security".into(),
                format!("{security}"),
                "Network-facing packages with an update waiting".into(),
            ));
        }

        let findings = self.shared.borrow().findings.get();
        let free = crate::maintenance::reclaimable_mb(&findings);
        if free != 0.0 {
            rows.push((
                "Free-able".into(),
                crate::maintenance::fmt_size(free),
                "Caches and logs that can be cleared".into(),
            ));
        }

        let kernel = Win::kernel_version();
        if !kernel.is_empty() {
            // The version, not the full release string: "7.2.5", not
            // "7.2.5-3-omarchy" -- the rest is in the tooltip.
            rows.push((
                "Kernel".into(),
                kernel.split('-').next().unwrap_or("").to_string(),
                format!("Running kernel release {kernel}"),
            ));
        }

        let up = Win::uptime()?;
        if !up.is_empty() {
            rows.push(("Up".into(), up, "Uptime, since last boot".into()));
        }

        for (n, (label, value, tip)) in rows.iter().enumerate() {
            let n = n as i32;
            self.stats.attach(&self.stat_row(label, value, tip), n % 2, n / 2, 1, 1);
        }
        Ok(())
    }

    pub fn kernel_version() -> String {
        Win::kernel_version_for(crate::demo::active())
    }

    /// `_kernel_version` with `demo.active()` passed in, so a test can ask
    /// for the demo answer without setting ALMANAC_DEMO under other tests.
    fn kernel_version_for(demo: bool) -> String {
        if demo {
            return "6.18.5".to_string(); // a screenshot must not name this machine's
        }
        let uts = rustix::system::uname();
        let release = uts.release().to_string_lossy();
        release.split("-arch").next().unwrap_or("").to_string()
    }

    /// Err where Python's `_uptime` raised past its `except (OSError,
    /// ValueError)`: an empty /proc/uptime (IndexError), or a NaN/infinite
    /// value reaching `int()` outside the try.
    pub fn uptime() -> Result<String, String> {
        Win::uptime_for(crate::demo::active())
    }

    /// `_uptime` with `demo.active()` passed in (see `kernel_version_for`).
    fn uptime_for(demo: bool) -> Result<String, String> {
        if demo {
            return Ok("6d 2h".to_string());
        }
        // A file that is not UTF-8 fails to decode: a ValueError, caught.
        let Ok(text) = std::fs::read_to_string("/proc/uptime") else {
            return Ok(String::new());
        };
        Win::uptime_from(&text)
    }

    /// The part of `_uptime` after the read, on the file's text.
    fn uptime_from(text: &str) -> Result<String, String> {
        let Some(first) = text.split(py_space).find(|p| !p.is_empty()) else {
            return Err("IndexError: list index out of range".to_string());
        };
        let Ok(seconds) = first.parse::<f64>() else {
            return Ok(String::new()); // float() ValueError, caught
        };
        if seconds.is_nan() {
            return Err("ValueError: cannot convert float NaN to integer".to_string());
        }
        if seconds.is_infinite() {
            return Err("OverflowError: cannot convert float infinity to integer".to_string());
        }
        let total = seconds.trunc() as i128;
        let (days, rem) = (total.div_euclid(86400), total.rem_euclid(86400));
        let (hours, minutes) = ((rem / 60).div_euclid(60), (rem / 60).rem_euclid(60));
        if days != 0 {
            return Ok(format!("{days}d {hours}h"));
        }
        if hours != 0 {
            return Ok(format!("{hours}h {minutes}m"));
        }
        Ok(format!("{minutes}m"))
    }

    /// Per-category totals, so the sidebar answers "what needs me?" at rest.
    pub fn update_sidebar_counts(&self) {
        let items = self.store_objects();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        counts.insert("updates".into(), Win::count_items(&items, |i| i.has_update));
        counts.insert(
            "apps".into(),
            Win::count_items(&items, |i| {
                (truthy(i.app.extra.get("is_app"))
                    || i.source == inventory::APPIMAGE
                    || i.source == inventory::LOCAL)
                    && i.source != inventory::ORPHAN
            }),
        );
        counts.insert("all".into(), Win::count_items(&items, |i| i.source != inventory::ORPHAN));
        for src in [
            inventory::SYSTEM,
            inventory::REPO,
            inventory::AUR,
            inventory::APPIMAGE,
            inventory::FLATPAK,
            inventory::LOCAL,
            inventory::AGENT,
            inventory::TOOL,
            inventory::ORPHAN,
        ] {
            counts.insert(src.into(), Win::count_items(&items, |i| i.source == src));
        }
        let count = |k: &str| counts.get(k).copied().unwrap_or(0);

        let (headers, rows) = {
            let sh = self.shared.borrow();
            let headers: Vec<(gtk::Image, Option<usize>)> = sh
                .group_rows
                .iter()
                .filter_map(|(gkey, header)| {
                    let header = header.as_ref()?;
                    let members = sh.group_members.get(gkey).cloned().unwrap_or_default();
                    let total = if sh.collapsed.contains(gkey) && members.len() > 1 {
                        Some(members.iter().map(|r| count(&r.key)).sum())
                    } else {
                        None
                    };
                    Some((header.chevron.clone(), total))
                })
                .collect();
            (headers, sh.sidebar_rows.clone())
        };
        for (chevron, total) in headers {
            match total {
                Some(total) => chevron.set_tooltip_text(Some(&format!("{total} items"))),
                None => chevron.set_tooltip_text(None),
            }
        }
        let security = Win::count_items(&items, |i| i.critical == "security");
        for row in rows {
            let key = row.key.as_str();
            if ["maintenance", "history", "plugins", "switchboard", inventory::AGENT, "featured", "themes"]
                .contains(&key)
            {
                continue; // their badges count findings, not packages
            }
            let n = count(key);
            // An empty category stays visible but recedes, so the layout does
            // not reshuffle as things are installed and removed.
            // "discover" has no inventory count of its own and "all"/"updates"
            // must stay reachable when empty, so none of the three is gated on
            // a count. Greying out the App Store is what made it unclickable.
            row.row.set_sensitive(
                n > 0
                    || [
                        "all",
                        "updates",
                        "discover",
                        "themes",
                        "plugins",
                        "switchboard",
                        "maintenance",
                        "featured",
                        inventory::AGENT,
                    ]
                    .contains(&key),
            );
            // Red means "a security update is waiting", nothing else. Pending
            // updates are a number, not an emergency.
            Win::badge(&row.count, n, if key == "updates" && security != 0 { "crit" } else { "" });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// test_sidebar_stats_are_synthetic
    #[test]
    fn sidebar_stats_are_synthetic() {
        // demo.active() is passed in rather than set in the environment:
        // other tests in this binary read ALMANAC_DEMO concurrently.
        let kernel = Win::kernel_version_for(true);
        let uptime = Win::uptime_for(true).unwrap_or_default();
        let real = rustix::system::uname().release().to_string_lossy().to_string();
        let real = real.split('-').next().unwrap_or("").to_string();
        if !real.is_empty() {
            assert!(!kernel.contains(&real));
        }
        assert_eq!(uptime, "6d 2h");
    }

    #[test]
    fn uptime_raises_where_python_does() {
        assert_eq!(Win::uptime_from("90061.5 1.0\n").as_deref(), Ok("1d 1h"));
        assert_eq!(Win::uptime_from("3720 0").as_deref(), Ok("1h 2m"));
        assert_eq!(Win::uptime_from("59").as_deref(), Ok("0m"));
        assert_eq!(Win::uptime_from("junk 1").as_deref(), Ok(""));
        assert!(Win::uptime_from("").unwrap_err().starts_with("IndexError"));
        assert!(Win::uptime_from("inf").unwrap_err().starts_with("OverflowError"));
        assert!(Win::uptime_from("nan").unwrap_err().starts_with("ValueError"));
    }
}
