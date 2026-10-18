//! UI section `switchboard` (port of `app.py` lines 3993-4221).
//!
//! The on/offs with no other home: startup apps, background services and
//! Almanac's own background work. Plugins are switched on the Plugins page,
//! beside their state and actions; here they get one row that says how many
//! are on and goes there.

use std::collections::BTreeMap;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use crate::autostart;
use crate::autoupdate;
use crate::history;
use crate::i18n::tr;
use crate::plugins as plugins_mod;
use crate::services;
use crate::util;
use crate::ui::shell::{self, exc_str, spawn_bg, Win};
use crate::watcher;

/// Switchboard state (`self.switch_body`, `self._switch_rows`).
#[derive(Default)]
pub struct State {
    /// `self.switch_body`: the column the sections are appended to.
    pub switch_body: Option<gtk::Box>,
    /// `self._switch_rows`: every filterable widget with the text it
    /// matches against (`(row, hay)`).
    pub switch_rows: Vec<(gtk::Widget, String)>,
}

/// What `apply(on)` reports: the state now in effect (`None` when it cannot
/// tell), or the `str()` of the exception it raised.
pub type ApplyResult = Result<Option<bool>, String>;

/// The keyword arguments of `_switch_row`.
pub struct SwitchOpts {
    pub prefix: Option<gtk::Widget>,
    /// `(label, css class)` pills shown before the switch.
    pub pills: Vec<(String, String)>,
    /// Asked before switching off something that matters; empty = no asking.
    pub confirm_off: String,
    pub sensitive: bool,
}

impl Default for SwitchOpts {
    fn default() -> Self {
        SwitchOpts { prefix: None, pills: Vec::new(), confirm_off: String::new(), sensitive: true }
    }
}

/// Python's `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// `str.splitlines()[-1]`.
fn py_last_line(s: &str) -> &str {
    s.rsplit(['\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}'])
        .next()
        .unwrap_or("")
}

/// Python's `repr(str)`.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_printable(c) => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable()` for one character.
fn py_printable(c: char) -> bool {
    let n = c as u32;
    !(c.is_control()
        || n == 0xa0
        || n == 0xad
        || n == 0x1680
        || (0x2000..=0x200f).contains(&n)
        || (0x2028..=0x202f).contains(&n)
        || (0x205f..=0x2064).contains(&n)
        || (0x2066..=0x206f).contains(&n)
        || n == 0x3000
        || n == 0xfeff
        || (0xe000..=0xf8ff).contains(&n)
        || (0xfff9..=0xfffb).contains(&n)
        || n >= 0xf0000)
}

fn service_apply(unit: &str, on: bool) -> ApplyResult {
    let argv = services::toggle_argv(unit, on);
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let r = util::run(&args, 60);
    if r.failed_to_start {
        return Err(format!("[Errno 2] No such file or directory: {}", py_repr(&argv[0])));
    }
    if r.timed_out {
        let list: Vec<String> = argv.iter().map(|a| py_repr(a)).collect();
        return Err(format!("Command '[{}]' timed out after 60 seconds", list.join(", ")));
    }
    if r.code != Some(0) {
        let text = py_strip(if !r.stderr.is_empty() { &r.stderr } else { &r.stdout });
        return Err(if !text.is_empty() { py_last_line(text).to_string() } else { "systemctl failed".to_string() });
    }
    // What is_enabled raises (output that does not decode, a NUL in the
    // name) is a ValueError: caught by the switch, which says why.
    Ok(Some(services::try_is_enabled(unit).map_err(|e| exc_str(&e))?))
}

fn autostart_apply(e: &autostart::Entry, on: bool) -> ApplyResult {
    autostart::set_enabled(e, on)?;
    Ok(autostart::entries()
        .into_iter()
        .find(|x| x.filename == e.filename)
        .map(|x| x.enabled))
}

fn guard(on: bool) -> ApplyResult {
    if on {
        watcher::enable(true)?;
    } else {
        watcher::disable()?;
    }
    Ok(Some(watcher::enabled()))
}

fn schedule(on: bool) -> ApplyResult {
    // What set_enabled / status raise, the switch reports as str(exc).
    let (ok, msg) = autoupdate::try_set_enabled(on).map_err(|e| exc_str(&e))?;
    if !ok {
        return Err(msg);
    }
    Ok(Some(autoupdate::try_status().map_err(|e| exc_str(&e))?.enabled))
}

impl Win {
    // ---- switchboard -------------------------------------------------------

    /// The on/offs with no other home: startup apps, background services
    /// and Almanac's own background work. Plugins are switched on the
    /// Plugins page, beside their state and actions; here they get one row
    /// that says how many are on and goes there.
    pub fn build_switchboard(&self) -> gtk::Widget {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let switch_body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(16)
            .build();
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&switch_body));
        page.append(&scroller);
        let old = {
            let mut st = self.switchboard.borrow_mut();
            (st.switch_body.replace(switch_body), std::mem::take(&mut st.switch_rows))
        };
        drop(old);
        page.upcast()
    }

    pub fn switchboard_reload(&self) {
        self.summarize(&tr("Reading what can be switched…"), Some("switchboard"));

        let w = self.downgrade();
        spawn_bg(
            // What raises in here ends the Python's worker before its
            // idle_add: the page is not rebuilt.
            || -> Result<_, String> {
                let on = plugins_mod::try_enabled()?;
                let starts = autostart::entries();
                let units = services::try_services()?;
                Ok((on, starts, units))
            },
            move |res| match res {
                Ok((on, starts, units)) => {
                    if let Some(w) = w.upgrade() {
                        w.switchboard_show(on, starts, units);
                    }
                }
                Err(e) => eprintln!("almanac: {e}"),
            },
        );
    }

    pub fn switchboard_show(
        &self,
        on: BTreeMap<String, bool>,
        starts: Vec<autostart::Entry>,
        units: Vec<services::Service>,
    ) -> glib::ControlFlow {
        let Some(switch_body) = self.switchboard.borrow().switch_body.clone() else {
            return glib::ControlFlow::Break;
        };
        while let Some(child) = switch_body.first_child() {
            switch_body.remove(&child);
        }
        let old = std::mem::take(&mut self.switchboard.borrow_mut().switch_rows);
        drop(old);

        let section = |title: String, note: String, rows: Vec<(adw::ActionRow, String)>| {
            if rows.is_empty() {
                return;
            }
            let bx = Win::section(&format!("{title} ({})", rows.len()), &note, None);
            for (row, text) in rows {
                bx.rows.append(&row);
                self.switchboard.borrow_mut().switch_rows.push((row.upcast(), text));
            }
            switch_body.append(&bx.root);
        };

        // Plugins: one row, not 47 duplicated from their own page.
        if !on.is_empty() {
            let row: adw::ActionRow = Win::plain(
                &tr("Plugins"),
                &format!(
                    "{} of {} on — switched on the \
                     Plugins page, beside their updates and shortcuts",
                    on.values().filter(|v| **v).count(),
                    on.len()
                ),
            );
            row.add_prefix(
                &gtk::Image::builder()
                    .icon_name("application-x-addon-symbolic")
                    .pixel_size(22)
                    .valign(gtk::Align::Center)
                    .build(),
            );
            row.add_suffix(
                &gtk::Image::builder()
                    .icon_name("go-next-symbolic")
                    .valign(gtk::Align::Center)
                    .build(),
            );
            row.set_activatable(true);
            let w = self.downgrade();
            row.connect_activated(move |_r| {
                if let Some(w) = w.upgrade() {
                    w.select_category("plugins");
                }
            });
            let links = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
            links.add_css_class("boxed-list");
            links.append(&row);
            switch_body.append(&links);
            self.switchboard
                .borrow_mut()
                .switch_rows
                .push((links.upcast(), "plugins".to_string()));
        }

        section(
            tr("Startup apps"),
            tr("Started when you log in. Switching one off writes a \
                per-user override; the system's copy is never touched."),
            starts
                .iter()
                .map(|e| (self.switch_autostart_row(e), format!("{} {} {}", e.name, e.filename, e.comment)))
                .collect(),
        );
        section(
            tr("Background services"),
            tr("Your session's systemd services and timers. Off stops it \
                now and at every login. Essential ones ask first."),
            units
                .iter()
                .map(|u| (self.switch_service_row(u), format!("{} {}", u.description, u.unit)))
                .collect(),
        );
        // `autoupdate.status()` raising stops `_switchboard_show` here.
        let almanac = match self.switch_almanac_rows() {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("almanac: {e}");
                return glib::ControlFlow::Break;
            }
        };
        section(tr("Almanac"), tr("Almanac's own background work."), almanac);

        let switches = starts.len() + units.len() + 2;
        self.summarize(
            &format!("{switches} switches · {} startup apps · {} services", starts.len(), units.len()),
            Some("switchboard"),
        );
        self.switchboard_filter(&self.search.text());
        glib::ControlFlow::Break
    }

    pub fn switchboard_filter(&self, text: &str) {
        let q = py_strip(text).to_lowercase();
        let rows = self.switchboard.borrow().switch_rows.clone();
        for (row, hay) in rows {
            row.set_visible(q.is_empty() || hay.to_lowercase().contains(&q));
        }
    }

    /// One row with a switch that moves only when the change holds.
    ///
    /// `apply(on)` runs off the UI thread and returns the state now in
    /// effect (or None when it cannot tell). `confirm_off` is asked before
    /// switching off something that matters.
    pub fn switch_row<F>(
        &self,
        title: &str,
        subtitle: &str,
        active: bool,
        apply: F,
        opts: SwitchOpts,
    ) -> adw::ActionRow
    where
        F: Fn(bool) -> ApplyResult + Send + Sync + 'static,
    {
        let SwitchOpts { prefix, pills, confirm_off, sensitive } = opts;
        let row: adw::ActionRow = Win::plain(title, subtitle);
        row.set_subtitle_lines(1);
        row.set_tooltip_text(Some(subtitle));
        if let Some(prefix) = prefix.as_ref() {
            row.add_prefix(prefix);
        }
        for (label, css) in &pills {
            let pill = gtk::Label::builder().label(label.as_str()).valign(gtk::Align::Center).build();
            pill.add_css_class(css);
            row.add_suffix(&pill);
        }
        let switch = gtk::Switch::builder()
            .active(active)
            .valign(gtk::Align::Center)
            .sensitive(sensitive)
            .build();
        shell::name(&switch, &if active { format!("{title} on") } else { format!("{title} off") });

        let title = title.to_string();
        let apply = Arc::new(apply);

        let done = {
            let switch = switch.downgrade();
            let title = title.clone();
            let w = self.downgrade();
            move |on: bool, now: Option<bool>, err: String| {
                let held = now.unwrap_or(!on);
                if let Some(switch) = switch.upgrade() {
                    switch.set_sensitive(sensitive);
                    switch.set_state(held);
                    switch.set_active(held);
                    shell::name(&switch, &if held { format!("{title} on") } else { format!("{title} off") });
                }
                let word = if on { "on" } else { "off" };
                if now == Some(on) {
                    if let Some(w) = w.upgrade() {
                        w.say(&format!("{title} {word}"), 5);
                    }
                    if let Err(e) =
                        history::record_checked(history::ALMANAC, &format!("{title} switched {word}"), "", "", &[])
                    {
                        eprintln!("almanac: {e}");
                    }
                } else if let Some(w) = w.upgrade() {
                    let body = if err.is_empty() { tr("The change did not take effect") } else { err };
                    w.notify(&format!("{title} was not switched {word}"), &body, "normal");
                }
            }
        };
        let done = std::rc::Rc::new(done);

        let go = {
            let switch = switch.downgrade();
            move |on: bool| {
                if let Some(switch) = switch.upgrade() {
                    switch.set_sensitive(false);
                }
                let apply = apply.clone();
                let done = done.clone();
                spawn_bg(
                    move || match apply(on) {
                        Ok(now) => (now, String::new()),
                        Err(exc) => (None, exc),
                    },
                    move |(now, err)| done(on, now, err),
                );
            }
        };
        let go = std::rc::Rc::new(go);

        let w = self.downgrade();
        switch.connect_state_set(move |sw, on| {
            if !on && !confirm_off.is_empty() {
                let dialog = adw::AlertDialog::new(
                    Some(&format!("Switch off {title}?")),
                    Some(&(format!("{confirm_off}\n\n") + &tr("You can switch it back on here."))),
                );
                dialog.add_response("cancel", &tr("Keep it on"));
                dialog.add_response("off", &tr("Switch off"));
                dialog.set_response_appearance("off", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                let go = go.clone();
                let sw = sw.downgrade();
                dialog.connect_response(None, move |_d, resp| {
                    if resp == "off" {
                        go(false);
                    } else if let Some(sw) = sw.upgrade() {
                        sw.set_active(true);
                    }
                });
                let parent = w.upgrade().map(|w| w.window.clone());
                dialog.present(parent.as_ref());
                return glib::Propagation::Stop;
            }
            go(on);
            glib::Propagation::Stop
        });
        row.add_suffix(&switch);
        row.set_activatable_widget(Some(&switch));
        row
    }

    pub fn switch_autostart_row(&self, e: &autostart::Entry) -> adw::ActionRow {
        let icon = gtk::Image::builder().pixel_size(28).valign(gtk::Align::Center).build();
        if e.icon.starts_with('/') {
            icon.set_from_file(Some(&e.icon));
        } else {
            icon.set_icon_name(Some(if e.icon.is_empty() {
                "application-x-executable-symbolic"
            } else {
                e.icon.as_str()
            }));
        }
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);

        let entry = e.clone();
        let apply = move |on: bool| autostart_apply(&entry, on);
        let subtitle = if e.comment.is_empty() { &e.filename } else { &e.comment };
        self.switch_row(
            &e.name,
            subtitle,
            e.enabled,
            apply,
            SwitchOpts { prefix: Some(icon.upcast()), ..Default::default() },
        )
    }

    pub fn switch_service_row(&self, u: &services::Service) -> adw::ActionRow {
        let mut pills = Vec::new();
        if !u.essential.is_empty() {
            pills.push((tr("Essential"), "pill-warn".to_string()));
        }
        if u.omarchy {
            pills.push((tr("Omarchy"), "pill-quiet".to_string()));
        }
        if u.active && u.enabled {
            pills.push((tr("Running"), "pill-ok".to_string()));
        }
        let mut warn = String::new();
        if !u.essential.is_empty() {
            warn = format!("Without it: {} stops working.", u.essential.to_lowercase());
        } else if u.omarchy {
            warn = tr("Omarchy set this up, and a later omarchy update may expect \
                       it to be on.");
        }

        let unit = u.unit.clone();
        let apply = move |on: bool| service_apply(&unit, on);
        let icon = gtk::Image::builder()
            .icon_name("system-run-symbolic")
            .pixel_size(22)
            .valign(gtk::Align::Center)
            .build();
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        self.switch_row(
            &u.description,
            &u.unit,
            u.enabled,
            apply,
            SwitchOpts { prefix: Some(icon.upcast()), pills, confirm_off: warn, ..Default::default() },
        )
    }

    /// `Err` where `autoupdate.status()` raises, after the first row is made.
    pub fn switch_almanac_rows(&self) -> Result<Vec<(adw::ActionRow, String)>, String> {
        let icon = || -> gtk::Widget {
            gtk::Image::builder()
                .icon_name("io.github.twodxme.Almanac")
                .pixel_size(28)
                .valign(gtk::Align::Center)
                .build()
                .upcast()
        };
        let guard_row = (
                self.switch_row(
                    &tr("Settings watcher"),
                    &tr("Notices settings reset to defaults, and plugin updates, \
                         while Almanac is closed"),
                    watcher::enabled(),
                    guard,
                    SwitchOpts { prefix: Some(icon()), ..Default::default() },
                ),
                "settings watcher guard".to_string(),
            );
        let enabled = autoupdate::try_status()?.enabled;
        Ok(vec![
            guard_row,
            (
                self.switch_row(
                    &tr("Scheduled updates"),
                    &tr("What runs and how often: Preferences → Updates"),
                    enabled,
                    schedule,
                    SwitchOpts { prefix: Some(icon()), ..Default::default() },
                ),
                "scheduled updates timer".to_string(),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SRC: &str = include_str!("switchboard.rs");

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn the_switch_means_now_and_at_login() {
        assert_eq!(
            services::toggle_argv("x.service", false),
            vec!["systemctl", "--user", "disable", "--now", "x.service"]
        );
    }

    /// test_spacing_comes_from_the_scale, against this file.
    #[test]
    fn spacing_comes_from_the_scale() {
        let scale = [0, 2, 4, 6, 8, 12, 16, 20, 24];
        let mut offenders = Vec::new();
        for prop in [".spacing(", ".margin_start(", ".margin_end(", ".margin_top(", ".margin_bottom("] {
            for part in SRC.split(prop).skip(1) {
                let num: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = num.parse::<i32>() {
                    if !scale.contains(&n) {
                        offenders.push(format!("{prop}{n}"));
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "spacing off the scale: {offenders:?}");
    }

    /// test_every_pill_class_is_defined_in_the_stylesheet, against this file.
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        let css = std::fs::read_to_string(root().join("data/style.css")).expect("style.css");
        let pills = ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"];
        let re = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#).expect("regex");
        let used: Vec<&str> =
            re.captures_iter(SRC).filter_map(|c| c.get(1).map(|m| m.as_str())).collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for name in &used {
            assert!(css.contains(&format!(".{name}")), "{name} is applied but never styled");
            assert!(pills.contains(name), "unknown pill variant {name}");
        }
    }

    /// test_every_icon_name_is_in_adwaita_or_shipped_here, against this file.
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return; // Adwaita icon theme not installed
        }
        fn stems(dir: &Path, out: &mut std::collections::BTreeSet<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
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
        let mut have = std::collections::BTreeSet::new();
        stems(adwaita, &mut have);
        stems(&root().join("../icons"), &mut have);
        let re = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#).expect("regex");
        let missing: Vec<&str> = re
            .captures_iter(SRC)
            .filter_map(|c| c.get(1).map(|m| m.as_str()))
            .filter(|n| !have.contains(*n) && !n.starts_with("adw-"))
            .collect();
        assert!(missing.is_empty(), "missing icons: {missing:?}");
    }
}
