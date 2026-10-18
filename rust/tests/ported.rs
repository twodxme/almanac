//! Tests from `../test_app.py` that had no faithful counterpart inside the
//! crate's own `#[cfg(test)]` modules.
//!
//! Three had none at all (`test_the_template_is_current`,
//! `test_nothing_dims_text_twice`, `test_the_table_sits_on_a_card_like_every_list`).
//! The rest are source-wide checks that the port split per UI section: each
//! section tests only its own file, so `ui/shell.rs` (sidebar, header, the
//! table) and the non-UI modules were never scanned, while the Python scans
//! the whole of `app.py` (or every `*.py`). These run them over the whole
//! crate, as the Python does.
//!
//! None of these needs a display: like the Python ones, they read the
//! source and the stylesheet. Nothing here writes outside a temp dir.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use almanac::ui::shell::CSS;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// Every source file of the app, with its test module cut off -- the Python skips
/// `test_app.py` the same way.
fn sources(under: &str) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&Path::new(ROOT).join(under), &mut files);
    files.sort();
    let tests = regex::Regex::new(r"(?m)^\s*#\[cfg\(test\)\]\s*\n\s*(?:pub )?mod \w+").unwrap();
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let code = match tests.find(&text) {
                Some(m) => text[..m.start()].to_string(),
                None => text,
            };
            (p, code)
        })
        .collect()
}

fn rel(p: &Path) -> String {
    p.strip_prefix(ROOT).unwrap_or(p).display().to_string()
}

fn ui_source() -> Vec<(PathBuf, String)> {
    sources("src/ui")
}

// ---------------------------------------------------------------- Translations

/// test_the_template_is_current: po/almanac.pot must hold every marked
/// string, or translators work from a stale list. Run po/update.sh after
/// changing UI text. (xgettext reads Rust as it reads Python; `tr` is `_`.)
#[test]
fn the_template_is_current() {
    if almanac::util::which("xgettext").is_none() {
        eprintln!("skipped: gettext is not installed");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let mut inputs = Vec::new();
    for (i, (p, code)) in sources("src").into_iter().enumerate() {
        // The test module is cut off, as test_app.py is left out.
        let f = d.path().join(format!("{i:03}-{}", p.file_name().unwrap().to_string_lossy()));
        std::fs::write(&f, code).unwrap();
        inputs.push(f);
    }
    let now = d.path().join("now.pot");
    let out = std::process::Command::new("xgettext")
        .args([
            "--language=Rust",
            "--keyword=tr",
            "--keyword=try_tr",
            "--keyword=ngettext:1,2",
            "--keyword=try_ngettext:1,2",
            "--from-code=UTF-8",
            "-o",
        ])
        .arg(&now)
        .args(&inputs)
        .output()
        .unwrap();
    assert!(out.status.success(), "xgettext: {}", String::from_utf8_lossy(&out.stderr));
    let re = regex::Regex::new(r#"(?m)^msgid "(.+)"$"#).unwrap();
    let ids = |f: &Path| -> BTreeSet<String> {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        re.captures_iter(&text).map(|c| c[1].to_string()).collect()
    };
    let pot = Path::new(ROOT).join("../po/almanac.pot");
    let missing: Vec<String> = ids(&now).difference(&ids(&pot)).cloned().collect();
    assert_eq!(missing, Vec::<String>::new(), "run po/update.sh");
}

// ---------------------------------------------------------------- TextContrast

/// test_nothing_dims_text_twice: a label with .dim-label must not also carry
/// its own opacity: the provenance line was faded to about 40% that way.
#[test]
fn nothing_dims_text_twice() {
    let rule = regex::Regex::new(r"([^{}]+)\{([^}]*)\}").unwrap();
    // (?<![-\w])opacity: the regex crate has no look-behind, so take the
    // character before and check it.
    let decl = regex::Regex::new(r"(^|[^-\w])opacity\s*:\s*([^;]+)").unwrap();
    for m in rule.captures_iter(CSS) {
        if let Some(d) = decl.captures(&m[2]) {
            assert!(
                d[2].contains("var(--dim-opacity)"),
                "{} sets its own opacity",
                m[1].trim()
            );
        }
    }
}

// ---------------------------------------------------------------- VisualSystem

const SCALE: [i64; 8] = [2, 4, 6, 8, 12, 16, 20, 24];
const PILLS: [&str; 6] = ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"];

/// test_the_table_sits_on_a_card_like_every_list: Updates, Applications and
/// Everything used the darker view background, edge to edge, while every
/// other page put its rows on a card. One look app-wide: the table is a card
/// too.
#[test]
fn the_table_sits_on_a_card_like_every_list() {
    let src: String = ui_source().into_iter().map(|(_, c)| c).collect();
    assert!(src.contains(r#"self.view.add_css_class("almanac-table")"#));
    assert!(src.contains(r#"scroller.add_css_class("card")"#));
    assert!(CSS.contains("columnview.almanac-table > listview"));
}

/// test_spacing_comes_from_the_scale, over every UI file (`app.py` whole).
#[test]
fn spacing_comes_from_the_scale() {
    // margin_start=12 / spacing=6 in PyGObject are `.margin_start(12)`,
    // `.set_margin_start(12)`, `.spacing(6)` and `Box::new(orientation, 6)`.
    // `\.` keeps column_spacing/row_spacing out, as `\b` does in the Python.
    let prop = regex::Regex::new(
        r"\.(?:set_)?(margin_(?:start|end|top|bottom)|spacing|child_spacing|line_spacing)\((\d+)\)",
    )
    .unwrap();
    let boxed = regex::Regex::new(r"Box::new\(\s*(?:gtk::)?Orientation::\w+,\s*(\d+)\s*\)").unwrap();
    let mut offenders: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for (p, code) in ui_source() {
        for c in prop.captures_iter(&code) {
            let v: i64 = c[2].parse().unwrap();
            if !SCALE.contains(&v) {
                offenders.entry(v).or_default().push(format!("{} {}", rel(&p), &c[1]));
            }
        }
        for c in boxed.captures_iter(&code) {
            let v: i64 = c[1].parse().unwrap();
            // Box::new(o, 0) is `Gtk.Box(orientation=o)`: no spacing written.
            if v != 0 && !SCALE.contains(&v) {
                offenders.entry(v).or_default().push(format!("{} spacing", rel(&p)));
            }
        }
    }
    assert_eq!(offenders, BTreeMap::new(), "spacing off the scale {SCALE:?}: {offenders:?}");
}

/// test_badges_use_one_pill_system: `app.py` + `store.py` -> every UI file
/// and `store.rs`.
#[test]
fn badges_use_one_pill_system() {
    let retired = [
        "crit-pill", "stale-pill", "crit-core", "store-chip", "store-installed",
        "trust-official", "trust-ok", "trust-warn", "trust-none",
    ];
    let mut src: String = ui_source().into_iter().map(|(_, c)| c).collect();
    for (p, c) in sources("src") {
        if p.file_name().is_some_and(|n| n == "store.rs") && p.parent().is_some_and(|d| d.ends_with("src")) {
            src.push_str(&c);
        }
    }
    let found: Vec<&str> = retired.into_iter().filter(|n| src.contains(&format!("\"{n}\""))).collect();
    assert_eq!(found, Vec::<&str>::new(), "retired badge classes still referenced: {found:?}");
}

/// test_every_pill_class_is_defined_in_the_stylesheet, over every UI file.
#[test]
fn every_pill_class_is_defined_in_the_stylesheet() {
    let re = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#).unwrap();
    let used: BTreeSet<String> = ui_source()
        .iter()
        .flat_map(|(_, c)| re.captures_iter(c).map(|m| m[1].to_string()).collect::<Vec<_>>())
        .collect();
    assert!(!used.is_empty(), "no pill classes found at all");
    for name in &used {
        assert!(CSS.contains(&format!(".{name}")), "{name} is applied but never styled");
    }
    let unknown: Vec<&String> = used.iter().filter(|n| !PILLS.contains(&n.as_str())).collect();
    assert!(unknown.is_empty(), "unknown pill variants: {unknown:?}");
}

// ---------------------------------------------------------------- Accessibility

/// test_every_icon_only_button_is_named, over every UI file. A button that is
/// only an icon is announced as just "button" unless it carries an
/// accessible name -- `shell::name()` sets it together with the tooltip, so
/// each one must reach `name` or set `Property::Label` itself within a few
/// lines of being created. A button kept on the window (`self.sort_btn` in
/// the Python) is built before `Inner` exists and named in `init`, so for
/// those the `name(&self.field` call may be anywhere in that file.
#[test]
fn every_icon_only_button_is_named() {
    let made = regex::Regex::new(
        r"(?:let (?:mut )?(\w+)(?::[^=]+)? = |^\s*(\w+): )(?:gtk::)?(?:Menu|Toggle)?Button::(builder\(\)|from_icon_name\()",
    )
    .unwrap();
    let mut unnamed = Vec::new();
    for (p, code) in ui_source() {
        let lines: Vec<&str> = code.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let Some(m) = made.captures(line) else { continue };
            let (var, field) = match (m.get(1), m.get(2)) {
                (Some(v), _) => (v.as_str(), false),
                (None, Some(v)) => (v.as_str(), true),
                _ => continue,
            };
            // Gtk.Button(icon_name=...): only a button that is nothing but
            // an icon -- no label, no child of its own.
            let chain: String = {
                let mut s = String::new();
                for l in &lines[i..(i + 12).min(lines.len())] {
                    s.push_str(l);
                    s.push('\n');
                    if l.contains(".build()") || l.contains("from_icon_name(") {
                        break;
                    }
                }
                s
            };
            let icon_only = &m[3] == "from_icon_name("
                || (chain.contains(".icon_name(") && !chain.contains(".label(") && !chain.contains(".child("));
            if !icon_only || ["upd", "play", "menu"].contains(&var) {
                continue; // named on bind
            }
            let window = lines[i..(i + 12).min(lines.len())].join("\n");
            let near = window.contains(&format!("name(&{var}")) || window.contains("Property::Label");
            // Kept on the window (a field, or a `let` moved into `Inner`):
            // named where `init` configures it.
            let anywhere = (field || code.contains(&format!("{var},")) || code.contains(&format!(": {var}")))
                && regex::Regex::new(&format!(r"name\(&(?:[\w()]+\.)+{var}\b")).unwrap().is_match(&code);
            if !near && !anywhere {
                unnamed.push(format!("{}:{} {var}", rel(&p), i + 1));
            }
        }
    }
    assert_eq!(unnamed, Vec::<String>::new());
}

// ---------------------------------------------------------------- IconsExist

/// test_every_icon_name_is_in_adwaita_or_shipped_here, over every module (the
/// Python reads every `*.py`, not only `app.py`). A theme-specific name
/// renders as a blank square on every other theme: Yaru has
/// emblem-ok-symbolic, Adwaita does not.
#[test]
fn every_icon_name_is_in_adwaita_or_shipped_here() {
    let adwaita = Path::new("/usr/share/icons/Adwaita");
    if !adwaita.is_dir() {
        eprintln!("skipped: Adwaita icon theme not installed");
        return;
    }
    fn stems(dir: &Path, deep: bool, exts: &[&str], out: &mut BTreeSet<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if deep {
                    stems(&p, deep, exts, out);
                }
            } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| exts.contains(&x)) {
                if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
                    out.insert(s.to_string());
                }
            }
        }
    }
    let mut have = BTreeSet::new();
    stems(adwaita, true, &["svg", "png"], &mut have);
    stems(&Path::new(ROOT).join("../icons"), false, &["svg"], &mut have);
    let re = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#).unwrap();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for (p, code) in sources("src") {
        for c in re.captures_iter(&code) {
            names.entry(c[1].to_string()).or_insert_with(|| rel(&p));
        }
    }
    // adw-* icons ship inside libadwaita itself.
    let missing: Vec<String> = names
        .iter()
        .filter(|(n, _)| !have.contains(*n) && !n.starts_with("adw-"))
        .map(|(n, f)| format!("{n} ({f})"))
        .collect();
    assert_eq!(missing, Vec::<String>::new());
}

// ---------------------------------------------------------------- AnyonesMachine

/// test_no_personal_paths_in_the_source, over every module (the port's copy
/// scans ui/actions.rs only).
#[test]
fn no_personal_paths_in_the_source() {
    let home = format!("{}/", almanac::util::home().display());
    for (p, code) in sources("src") {
        // Whoever runs the tests: their home must never be baked in.
        assert!(!code.contains(&home), "{}", rel(&p));
        assert!(!code.to_lowercase().contains("streamdock"), "{}", rel(&p));
    }
}
