//! Pure decisions, kept apart from the widgets so they can be tested without
//! a display: matching, folding groups, badges, sorting.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde_json::Value;

use crate::inventory;
use crate::ui::shell::{AppItem, lower_cmp, reverse_text};

use super::extra::{PyInt, py_int, py_space, py_type_name, truthy};

/// The body of `_match` for one row, given the window's filter and query.
pub fn match_item(filter_key: &str, query: &str, item: &AppItem) -> bool {
    if item.source == inventory::AVAILABLE || item.source == inventory::PLUGIN {
        return true;
    }
    if filter_key == "updates" && !item.has_update {
        return false;
    }
    if filter_key == "apps"
        && !(truthy(item.app.extra.get("is_app"))
            || item.source == inventory::APPIMAGE
            || item.source == inventory::LOCAL)
    {
        return false;
    }
    // Plugins have their own page. They stay in Everything and Updates.
    if truthy(item.app.extra.get("plugin_id"))
        && (filter_key == "apps" || filter_key == inventory::LOCAL)
    {
        return false;
    }
    let broad = matches!(filter_key, "all" | "updates" | "apps");
    if broad && item.source == inventory::ORPHAN {
        return false;
    }
    if !broad && item.source != filter_key {
        return false;
    }
    if !query.is_empty() && !item.haystack.contains(query) {
        return false;
    }
    true
}

/// What `_apply_group_state` does to one group, worked out from the
/// sidebar's tables: the header's chevron icon and each member row's
/// visibility. The widgets are generic so the wiring can be tested without
/// a display.
pub struct GroupState<H, M> {
    pub header: H,
    pub icon: &'static str,
    pub members: Vec<(M, bool)>,
}

pub fn group_state<H: Clone, M: Clone>(
    group_rows: &[(String, Option<H>)],
    group_members: &BTreeMap<String, Vec<M>>,
    collapsed: &std::collections::BTreeSet<String>,
    filter_key: &str,
    gkey: &str,
    key_of: impl Fn(&M) -> &str,
) -> Option<GroupState<H, M>> {
    // self.group_rows.get(gkey) is None: ungrouped primaries never fold.
    let header = group_rows.iter().find(|(k, _)| k == gkey).and_then(|(_, h)| h.clone())?;
    let folded = collapsed.contains(gkey);
    let members = group_members
        .get(gkey)
        .map(|rows| {
            rows.iter()
                // The selected category stays visible even when its group
                // folds, or the sidebar would show a selection that is not
                // there.
                .map(|r| (r.clone(), !folded || key_of(r) == filter_key))
                .collect()
        })
        .unwrap_or_default();
    Some(GroupState {
        header,
        icon: if folded { "pan-end-symbolic" } else { "pan-down-symbolic" },
        members,
    })
}

/// The badges `_detail_badges` puts under an app's name.
pub fn detail_badges_for(item: &AppItem) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    if item.critical == "security" {
        out.push(("security update".to_string(), "pill-crit"));
    } else if item.critical == "core" {
        out.push(("core system".to_string(), "pill-crit"));
    }
    if item.stale == "orphaned" {
        out.push(("unmaintained".to_string(), "pill-warn"));
    } else if item.stale == "flagged" {
        out.push(("flagged out of date".to_string(), "pill-warn"));
    }
    if item.git_dirty {
        out.push(("local edits".to_string(), "pill-warn"));
    }
    if item.source == inventory::ORPHAN {
        out.push((format!("orphan · {}", item.advice), "pill-warn"));
    }
    out
}

/// The "Available" row of the detail sheet: the new version and how far
/// behind the installed one is.
pub fn available_label(item: &AppItem) -> String {
    let mut label = item.app.update.clone();
    if item.behind != 0 && matches!(item.behind_kind.as_str(), "major" | "minor" | "patch" | "point")
    {
        label += &format!("  ({} {}", item.behind, item.behind_kind);
        label += if item.behind > 1 { "s behind)" } else { " behind)" };
    } else if item.behind_kind == "rel" {
        label += "  (rebuild only)";
    }
    label
}

/// Keys `_on_sort` has a getter for.
pub const SORT_KEYS: &[&str] = &[
    "name", "name_desc", "size", "installed", "category", "behind", "popular", "trending",
    "newest",
];

/// `-int(i.added.replace("-", "") or 0)` when `added` is set, "" when not --
/// and None where Python's int() would raise.
#[derive(Debug, Clone, PartialEq)]
enum Added {
    Blank,
    Neg(PyInt),
}

fn added_key(added: &str) -> Option<Added> {
    if added.is_empty() {
        return Some(Added::Blank);
    }
    let digits = added.replace('-', "");
    if digits.is_empty() {
        return Some(Added::Neg(PyInt { neg: false, mag: "0".into() }));
    }
    py_int(&digits).map(|n| Added::Neg(n.negated()))
}

fn float_cmp(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// `(g(a) > g(b)) - (g(a) < g(b))` for the getter `key` names. A getter
/// that would raise in Python (an unparseable date, or "" compared with a
/// number) makes the pair compare equal, as the failed callback did.
pub fn sort_cmp(key: &str, a: &AppItem, b: &AppItem) -> Ordering {
    // Lowered as they are compared, not up front: this runs on every
    // comparison, and most stop before the name.
    let names = || lower_cmp(&a.name, &b.name);
    match key {
        "name" => names(),
        "name_desc" => names().reverse(),
        "size" => float_cmp(-a.size_mb, -b.size_mb),
        "installed" => float_cmp(-a.installed_ts, -b.installed_ts),
        "category" => (a.category.is_empty())
            .cmp(&b.category.is_empty())
            .then_with(|| lower_cmp(&a.category, &b.category)),
        // Negated in i128: Python ints are unbounded, so -i64::MIN is just a
        // bigger number there, not an overflow.
        "behind" => (-i128::from(a.behind)).cmp(&-i128::from(b.behind)),
        // All-time standing. Unverified plugins sink: anyone can list one.
        "popular" => (-i128::from(a.votes), !a.verified)
            .cmp(&(-i128::from(b.votes), !b.verified))
            .then_with(names),
        // The AUR's popularity is already decay-weighted, which is what
        // "trending" means; plugins fall back to how recently they landed.
        "trending" => {
            let (Some(xa), Some(xb)) = (added_key(&a.added), added_key(&b.added)) else {
                return Ordering::Equal;
            };
            // The tuple compare stops at the first element that is not ==.
            // A NaN is never == anything, so it stops there, and NaN is
            // neither > nor < anything: the pair compares equal outright.
            let (pa, pb) = (-a.popularity, -b.popularity);
            if pa.is_nan() || pb.is_nan() {
                return Ordering::Equal;
            }
            let first = float_cmp(pa, pb);
            if first != Ordering::Equal {
                return first;
            }
            let second = match (&xa, &xb) {
                (Added::Blank, Added::Blank) => Ordering::Equal,
                (Added::Neg(x), Added::Neg(y)) => x.cmp(y),
                _ => return Ordering::Equal, // str vs int: TypeError
            };
            second.then_with(names)
        }
        "newest" => (a.added.is_empty())
            .cmp(&b.added.is_empty())
            .then_with(|| reverse_text(&a.added, &b.added))
            .then_with(names),
        _ => Ordering::Equal,
    }
}

/// `" ".join(text.split())[:400]`.
pub(super) fn squeeze(text: &str, limit: usize) -> String {
    text.split(py_space)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

/// `len(sources or [])` of the plugin registry, for the Discover summary: a
/// falsy value counts as the empty list; a truthy one without a len() (a
/// number, True) is the TypeError that ended `_show_discover` before the
/// summary.
pub(super) fn discover_total(registry: &Value) -> Result<usize, String> {
    Ok(match registry.get("sources") {
        Some(v) if !truthy(Some(v)) => 0,
        None => 0,
        Some(Value::Array(a)) => a.len(),
        Some(Value::Object(o)) => o.len(),
        Some(Value::String(s)) => s.chars().count(),
        Some(other) => {
            return Err(format!("TypeError: object of type '{}' has no len()", py_type_name(other)));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::packages::thousands;

    fn item(name: &str, source: &str, extra: Value) -> AppItem {
        let mut app = inventory::App::new(name, "1", source);
        if let Value::Object(m) = extra {
            app.extra = m.into();
        }
        AppItem::new(app)
    }

    /// test_plugins_leave_applications_and_local_builds
    #[test]
    fn plugins_leave_applications_and_local_builds() {
        let mut plugin = item("p", inventory::LOCAL, serde_json::json!({"plugin_id": "p"}));
        plugin.has_update = true;
        plugin.haystack = String::new();
        let mut build = item("b", inventory::LOCAL, serde_json::json!({}));
        build.haystack = String::new();
        for (key, want) in
            [("apps", false), (inventory::LOCAL, false), ("all", true), ("updates", true)]
        {
            assert_eq!(match_item(key, "", &plugin), want, "{key}");
        }
        assert!(match_item(inventory::LOCAL, "", &build));
    }

    #[test]
    fn installable_rows_always_match_and_orphans_stay_on_their_page() {
        let avail = item("x", inventory::AVAILABLE, serde_json::json!({}));
        assert!(match_item("updates", "nothing", &avail));
        let orphan = item("o", inventory::ORPHAN, serde_json::json!({}));
        assert!(!match_item("all", "", &orphan));
        assert!(match_item(inventory::ORPHAN, "", &orphan));
        let repo = item("Firefox", inventory::REPO, serde_json::json!({}));
        assert!(match_item("all", "firefox", &repo));
        assert!(!match_item("all", "chromium", &repo));
        assert!(!match_item(inventory::AUR, "", &repo));
    }

    #[test]
    fn detail_badges_and_versions() {
        let mut it = item("x", inventory::ORPHAN, serde_json::json!({"critical": "security",
                                                                       "stale": "flagged",
                                                                       "advice": "keep"}));
        it.git_dirty = true;
        let got: Vec<(String, &str)> = detail_badges_for(&it);
        assert_eq!(
            got,
            vec![
                ("security update".to_string(), "pill-crit"),
                ("flagged out of date".to_string(), "pill-warn"),
                ("local edits".to_string(), "pill-warn"),
                ("orphan · keep".to_string(), "pill-warn"),
            ]
        );
        let mut it = AppItem::default();
        it.app.update = "2.0".into();
        it.behind = 2;
        it.behind_kind = "major".into();
        assert_eq!(available_label(&it), "2.0  (2 majors behind)");
        it.behind = 1;
        assert_eq!(available_label(&it), "2.0  (1 major behind)");
        it.behind_kind = "rel".into();
        assert_eq!(available_label(&it), "2.0  (rebuild only)");
    }

    #[test]
    fn files_get_thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(-1234), "-1,234");
        assert_eq!(squeeze("a  b\n c", 3), "a b");
    }

    #[test]
    fn sort_getters_order_like_the_python() {
        let mut a = AppItem { name: "Alpha".into(), ..Default::default() };
        let mut b = AppItem { name: "beta".into(), ..Default::default() };
        assert_eq!(sort_cmp("name", &a, &b), Ordering::Less);
        assert_eq!(sort_cmp("name_desc", &a, &b), Ordering::Greater);
        a.size_mb = 10.0;
        b.size_mb = 20.0;
        assert_eq!(sort_cmp("size", &a, &b), Ordering::Greater); // biggest first
        a.category = String::new();
        b.category = "Office".into();
        assert_eq!(sort_cmp("category", &a, &b), Ordering::Greater); // blanks last
        a.verified = true;
        assert_eq!(sort_cmp("popular", &a, &b), Ordering::Less);
        a.added = "2024-01-02".into();
        b.added = "2024-03-01".into();
        assert_eq!(sort_cmp("newest", &a, &b), Ordering::Greater);
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Greater);
        b.added = String::new();
        // "" against a number raised TypeError: the pair compares equal.
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Equal);
        assert!(SORT_KEYS.iter().all(|k| *k != "default"));
    }

    /// Review fixes: NaN popularity, unbounded negation, Python whitespace.
    #[test]
    fn sort_and_text_edge_cases_follow_python() {
        let mut a = AppItem { name: "a".into(), ..Default::default() };
        let b = AppItem { name: "b".into(), ..Default::default() };
        a.popularity = f64::NAN;
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Equal);
        assert_eq!(sort_cmp("trending", &b, &a), Ordering::Equal);
        a.popularity = 0.0;
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Less);
        a.behind = i64::MIN;
        assert_eq!(sort_cmp("behind", &a, &b), Ordering::Greater);
        a.votes = i64::MIN;
        assert_eq!(sort_cmp("popular", &a, &b), Ordering::Greater);
        assert_eq!(squeeze("x\u{1c}y  z\n", 400), "x y z");
        assert_eq!(squeeze("abcdef", 3), "abc");
    }
}
