//! UI section `preferences` (port of `app.py` lines 4876-5096): the
//! Preferences dialog and "Run the update now".

use std::rc::Rc;

use adw::prelude::*;
use serde_json::Value;

use crate::i18n::tr;
use crate::ui::shell::{spawn_bg, Win};
use crate::{autoupdate, plugins as plugins_mod, settings, tasks, watcher, xdg};

/// This section keeps nothing of its own: `self._plugins is None` is
/// shared state (`shell::Shared::plugins_none`).
#[derive(Default)]
pub struct State {}

type After = Rc<dyn Fn()>;

/// What the Updates page shows about the scheduled run, read off the main
/// thread before the dialog is built.
struct Scheduled {
    cfg: autoupdate::Config,
    st: autoupdate::Status,
    /// The schedule key in effect (`autoupdate::current_schedule`).
    current: String,
    /// Old units in ~/.config, and what became of them.
    legacy: autoupdate::Legacy,
    /// The scheduled run installs with `omarchy update`.
    omarchy: bool,
    /// Demo mode: fixtures shown, nothing saved, no unit touched.
    demo: bool,
}

impl Scheduled {
    /// The machine's own state. Old units Almanac installed before the
    /// package shipped them are replaced first (see
    /// `autoupdate::migrate_legacy_units`), so what is shown is what runs.
    fn read() -> Result<Scheduled, String> {
        if crate::demo::active() {
            return Ok(Scheduled::demo());
        }
        let legacy = autoupdate::migrate_legacy_units().unwrap_or_else(|e| {
            eprintln!("almanac: {e}");
            autoupdate::Legacy::None
        });
        Ok(Scheduled {
            cfg: autoupdate::load()?,
            st: autoupdate::try_status()?,
            current: autoupdate::current_schedule()?,
            legacy,
            omarchy: autoupdate::omarchy_installed(),
            demo: false,
        })
    }

    /// Screenshots: a timer that is on, with made-up times, nothing read
    /// from this machine's systemd.
    fn demo() -> Scheduled {
        Scheduled {
            cfg: autoupdate::DEFAULTS.clone(),
            st: autoupdate::Status {
                enabled: true,
                next: "Sun 2026-11-01 11:14:02 UTC".into(),
                last: "Sun 2026-10-25 11:06:51 UTC".into(),
            },
            current: "weekly".into(),
            legacy: autoupdate::Legacy::None,
            omarchy: true,
            demo: true,
        }
    }
}

fn switch(
    group: &adw::PreferencesGroup,
    key: &'static str,
    title: &str,
    subtitle: &str,
    after: Option<After>,
) -> Result<adw::SwitchRow, String> {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(settings::get_bool(key))
        .build();

    row.connect_active_notify(move |r| {
        if let Err(e) = settings::set(key, Value::Bool(r.is_active())) {
            eprintln!("almanac: {e}");
            return;
        }
        if let Some(after) = &after {
            after();
        }
    });
    group.add(&row);
    Ok(row)
}

// ---- settings ---------------------------------------------------------
impl Win {
    /// Almanac's preferences: General, Updates, Privacy, Background.
    ///
    /// Scheduled-update changes are applied when the dialog closes (one
    /// daemon-reload, not one per switch); everything else on change.
    ///
    /// The timer's state is systemctl's to tell: asked off the main thread,
    /// and the dialog built when it answers.
    pub fn on_preferences(&self) {
        let w = self.downgrade();
        spawn_bg(
            Scheduled::read,
            move |read| match read {
                Ok(sched) => {
                    if let Some(w) = w.upgrade() {
                        if let Err(e) = w.build_preferences(sched) {
                            eprintln!("almanac: {e}");
                        }
                    }
                }
                Err(e) => eprintln!("almanac: {e}"),
            },
        );
    }

    fn build_preferences(&self, sched_state: Scheduled) -> Result<(), String> {
        let Scheduled { cfg, st, current, legacy, omarchy, demo } = sched_state;
        let dialog = adw::PreferencesDialog::builder().title(tr("Preferences")).build();

        // ---- General
        let general = adw::PreferencesPage::builder()
            .title(tr("General"))
            .icon_name("preferences-system-symbolic")
            .build();
        let start = adw::PreferencesGroup::builder().title(tr("Starting up")).build();
        let pages = settings::START_PAGES;
        let page_labels: Vec<String> = pages.iter().map(|(_k, l)| tr(l)).collect();
        let page_label_refs: Vec<&str> = page_labels.iter().map(String::as_str).collect();
        let start_row = adw::ComboRow::builder()
            .title(tr("Open on"))
            .model(&gtk::StringList::new(&page_label_refs))
            .build();
        let page_keys: Vec<&'static str> = pages.iter().map(|(k, _l)| *k).collect();
        let cur = settings::get_str("start_page");
        start_row.set_selected(page_keys.iter().position(|k| *k == cur).unwrap_or(0) as u32);
        {
            let page_keys = page_keys.clone();
            start_row.connect_selected_notify(move |r| {
                let Some(k) = page_keys.get(r.selected() as usize) else {
                    eprintln!("almanac: IndexError: list index out of range");
                    return;
                };
                if let Err(e) = settings::set("start_page", Value::String((*k).to_string())) {
                    eprintln!("almanac: {e}");
                }
            });
        }
        start.add(&start_row);
        general.add(&start);

        let notes = adw::PreferencesGroup::builder()
            .title(tr("Notifications"))
            .description(tr(
                "Desktop notifications. Results still show inside the window either way.",
            ))
            .build();
        switch(
            &notes,
            "notify_no_change",
            &tr("When an action changes nothing"),
            &tr("An update or cleanup that ran but left things as they were"),
            None,
        )?;
        switch(
            &notes,
            "notify_resets",
            &tr("When settings are reset to defaults"),
            &tr("A tool replaced one of your config files with its default"),
            None,
        )?;
        switch(
            &notes,
            "notify_plugin_updates",
            &tr("When plugin updates are available"),
            &tr("Checked every few hours by the settings watcher (Background)"),
            None,
        )?;
        general.add(&notes);
        dialog.add(&general);

        // ---- Updates
        let updates = adw::PreferencesPage::builder()
            .title(tr("Updates"))
            .icon_name("software-update-available-symbolic")
            .build();
        let sched = adw::PreferencesGroup::builder()
            .title(tr("Automatic updates"))
            .description(tr(
                "A systemd user timer runs the update in the background. Anything that could \
                 stop the machine booting is always held for you to run by hand, whatever is \
                 set here.",
            ))
            .build();
        let enabled = adw::SwitchRow::builder()
            .title(tr("Run updates on a schedule"))
            .active(st.enabled)
            .build();
        // It installs with `omarchy update`: on plain Arch there is nothing
        // to switch on (a timer switched on some other way can still be
        // switched off).
        if !omarchy {
            enabled.set_subtitle(&autoupdate::needs_omarchy());
            enabled.set_sensitive(st.enabled);
        }
        sched.add(&enabled);
        if let autoupdate::Legacy::Foreign(paths) = &legacy {
            let shown: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            // Not Almanac's: they override the packaged units, and are left
            // alone.
            let body = format!(
                "{}\n{}",
                tr("Almanac leaves it alone, and it overrides the one Almanac ships, so these \
                    settings may not apply. Remove it to use Almanac's:"),
                shown.join("\n")
            );
            let note = Win::note_row("dialog-information-symbolic", &tr("Your own copy of the timer is in use"), &body);
            sched.add(&note);
        }
        // Its own names: the plugin-scope list below once reused "keys", and
        // closing the dialog saved a scope ("verified") as the schedule.
        let sched_labels: Vec<&str> = autoupdate::SCHEDULES.iter().map(|(_k, label, _c)| *label).collect();
        let sched_keys: Vec<&'static str> = autoupdate::SCHEDULES.iter().map(|(k, _l, _c)| *k).collect();
        let combo = adw::ComboRow::builder()
            .title(tr("How often"))
            .model(&gtk::StringList::new(&sched_labels))
            .build();
        combo.set_selected(sched_keys.iter().position(|k| *k == current).unwrap_or(1) as u32);
        sched.add(&combo);
        for (title, value) in [(tr("Next run"), st.next.as_str()), (tr("Last run"), st.last.as_str())] {
            let r = adw::ActionRow::builder().title(title).subtitle(value).build();
            r.set_subtitle_selectable(true);
            sched.add(&r);
        }
        updates.add(&sched);

        let include = adw::PreferencesGroup::builder()
            .title(tr("What the scheduled run updates"))
            .description(tr(
                "Each of these updates independently. Turning one off does not stop you \
                 updating it by hand here.",
            ))
            .build();
        let mut rows: Vec<(&'static str, adw::SwitchRow)> = Vec::new();
        for (key, title, subtitle) in [
            (
                "packages",
                "Repository packages",
                "Needs passwordless sudo to apply; without it the run only notifies you that \
                 updates are waiting.",
            ),
            (
                "aur",
                "Also update AUR packages",
                "Off unless you turn it on. An AUR package is built from a script anyone can \
                 publish, and installing it runs that script as root with nobody reading it \
                 first. Left off, the run tells you about AUR updates and installs nothing.",
            ),
            (
                "agents",
                "AI agent CLIs",
                "Claude Code, Codex and the rest. They replace their own binaries and need no \
                 root.",
            ),
            (
                "appimages",
                "Tracked AppImages",
                "Downloads the newer build and keeps the previous one so an update can be \
                 reverted.",
            ),
            (
                "plugins",
                "Omarchy shell plugins",
                "Off by default: a plugin update changes the desktop while you are using it.",
            ),
        ] {
            let row = adw::SwitchRow::builder()
                .title(title)
                .subtitle(subtitle)
                .active(cfg.get_bool(key))
                .build();
            rows.push((key, row.clone()));
            include.add(&row);
        }
        // AUR updates ride on package updates: off with them.
        if let (Some((_, packages)), Some((_, aur))) =
            (rows.iter().find(|(k, _)| *k == "packages"), rows.iter().find(|(k, _)| *k == "aur"))
        {
            packages.bind_property("active", aur, "sensitive").sync_create().build();
        }
        updates.add(&include);

        let care = adw::PreferencesGroup::builder().title(tr("Care")).build();
        switch(
            &care,
            "hold_majors",
            &tr("Hold back major releases"),
            &tr("A new major version of an agent CLI waits for you to update it on its own, \
                 with its release notes"),
            None,
        )?;
        let scope_keys: Vec<&'static str> = plugins_mod::SCOPES.iter().map(|(k, _l, _s)| *k).collect();
        let scope_labels: Vec<&str> = plugins_mod::SCOPES.iter().map(|(_k, lbl, _s)| *lbl).collect();
        let scope_row = adw::ComboRow::builder()
            .title(tr("Plugins that bulk updates touch"))
            .model(&gtk::StringList::new(&scope_labels))
            .build();
        // `scope()` only ever returns one of SCOPES' keys.
        let scope_now = plugins_mod::scope();
        scope_row.set_selected(scope_keys.iter().position(|k| *k == scope_now).unwrap_or(0) as u32);
        scope_row.connect_selected_notify(move |r| {
            let Some(k) = scope_keys.get(r.selected() as usize) else {
                eprintln!("almanac: IndexError: list index out of range");
                return;
            };
            if let Err(e) = plugins_mod::set_scope(k) {
                eprintln!("almanac: {e}");
            }
        });
        care.add(&scope_row);
        updates.add(&care);

        let actions = adw::PreferencesGroup::new();
        let run_row = adw::ActionRow::builder()
            .title(tr("Run the update now"))
            .subtitle(tr("Runs exactly what the schedule would run, in a window you can watch."))
            .build();
        let run_btn = gtk::Button::builder()
            .label(tr("Run now"))
            .valign(gtk::Align::Center)
            .build();
        run_btn.set_tooltip_text(Some(&tr(
            "Run the scheduled update immediately, in a window you can watch",
        )));
        run_btn.add_css_class("suggested-action");
        // Demo mode runs nothing; without Omarchy there is nothing it could
        // install.
        if demo || !omarchy {
            run_btn.set_sensitive(false);
            if !omarchy {
                run_row.set_subtitle(&autoupdate::needs_omarchy());
            }
        }
        {
            let w = self.downgrade();
            run_btn.connect_clicked(move |_b| {
                if let Some(w) = w.upgrade() {
                    w.run_scheduled_now();
                }
            });
        }
        run_row.add_suffix(&run_btn);
        actions.add(&run_row);
        let log_row = adw::ActionRow::builder()
            .title(tr("Last run log"))
            .subtitle(
                xdg::try_state_home()?
                    .join("omarchy-auto-update/last-run.log")
                    .to_string_lossy()
                    .as_ref(),
            )
            .build();
        log_row.set_subtitle_selectable(true);
        actions.add(&log_row);
        updates.add(&actions);
        dialog.add(&updates);

        // ---- Privacy
        let privacy = adw::PreferencesPage::builder()
            .title(tr("Privacy"))
            .icon_name("preferences-system-privacy-symbolic")
            .build();
        let net = adw::PreferencesGroup::builder()
            .title(tr("What Almanac fetches"))
            .description(tr(
                "Each is a request to the named service, made only to show you something. \
                 Nothing about this machine is sent with any of them. Switch one off and that \
                 part of Almanac does without.",
            ))
            .build();
        let reload_store: After = {
            let w = self.downgrade();
            Rc::new(move || {
                if let Some(w) = w.upgrade() {
                    // `setattr(self, "_plugins", None)`
                    let mut sh = w.shared.borrow_mut();
                    sh.plugins_none = true;
                    sh.plugins = Default::default();
                }
            })
        };
        switch(
            &net,
            "net_market_omarchy",
            &tr("Omarchy plugin marketplace"),
            "plugins.omarchy.org",
            Some(reload_store.clone()),
        )?;
        switch(
            &net,
            "net_market_themes",
            &tr("Omarchy themes"),
            &tr("GitHub, the omarchy-theme topic"),
            Some(reload_store.clone()),
        )?;
        switch(
            &net,
            "net_market_artcraft",
            &tr("ArtCraft apps"),
            &tr("getartcraft.com and its GitHub releases"),
            Some(reload_store),
        )?;
        switch(
            &net,
            "net_security_tracker",
            &tr("Arch Security Tracker"),
            &tr("security.archlinux.org — the whole advisory list, matched here"),
            None,
        )?;
        let news_after: After = {
            let w = self.downgrade();
            Rc::new(move || {
                if let Some(w) = w.upgrade() {
                    w.news_setting_changed();
                }
            })
        };
        switch(
            &net,
            "net_arch_news",
            &tr("Check Arch news"),
            &tr("archlinux.org/news, at most once an hour — where updates that need manual \
                 steps are announced"),
            Some(news_after),
        )?;
        let picks_after: After = {
            let w = self.downgrade();
            Rc::new(move || {
                if let Some(w) = w.upgrade() {
                    if settings::get_bool("net_daily_picks") {
                        w.feat_reload();
                    } else {
                        w.picks_show(Vec::new());
                    }
                }
            })
        };
        switch(
            &net,
            "net_daily_picks",
            &tr("Today's picks"),
            &tr("GitHub: recent stars, forks, downloads and comments for a shortlist of about \
                 ten listings, once a day"),
            Some(picks_after),
        )?;
        switch(
            &net,
            "net_vendor_icons",
            &tr("Logos"),
            &tr("Each from its vendor's own site, or GitHub for avatars; logos already saved \
                 still show"),
            None,
        )?;
        switch(
            &net,
            "net_gh_token",
            &tr("Use your GitHub sign-in"),
            &tr("Your existing gh login raises GitHub's limit from 60 requests an hour to \
                 5,000. Held in memory, never saved or logged."),
            None,
        )?;
        privacy.add(&net);
        dialog.add(&privacy);

        // ---- Background
        let background = adw::PreferencesPage::builder()
            .title(tr("Background"))
            .icon_name("emblem-system-symbolic")
            .build();
        let watch = adw::PreferencesGroup::builder()
            .title(tr("While Almanac is closed"))
            .description(tr(
                "A small background process with no window. It watches config folders for a \
                 reset to defaults, and checks plugins for updates every few hours.",
            ))
            .build();
        let guard_row = adw::SwitchRow::builder()
            .title(tr("Settings watcher"))
            .subtitle(tr("Starts with your session, through the standard autostart folder"))
            .active(watcher::enabled())
            .build();

        {
            let w = self.downgrade();
            guard_row.connect_active_notify(move |row| {
                let result = if row.is_active() { watcher::enable(true) } else { watcher::disable() };
                if let Err(exc) = result {
                    if let Some(w) = w.upgrade() {
                        w.say(&format!("Could not change the autostart entry: {exc}"), 5);
                    }
                }
            });
        }
        watch.add(&guard_row);
        background.add(&watch);
        dialog.add(&background);

        let w = self.downgrade();
        let st_enabled = st.enabled;
        // The saves need no window: only the toasts and the store reload do,
        // so a window already gone still gets its settings written.
        dialog.connect_closed(move |_d| {
            // Demo mode: the switches were for show.
            if demo {
                return;
            }
            let mut new = cfg.clone();
            for (key, row) in &rows {
                new.set(key, row.is_active());
            }
            let Some(&want) = sched_keys.get(combo.selected() as usize) else {
                eprintln!("almanac: IndexError: list index out of range");
                return;
            };
            new.set("schedule", want);
            if let Err(e) = autoupdate::save(&new) {
                eprintln!("almanac: {e}");
                return;
            }
            // The timer changes are systemctl calls (a daemon-reload among
            // them): made off the main thread, reported when they are done.
            let schedule = (want != current).then(|| want.to_string());
            let timer = (enabled.is_active() != st_enabled).then(|| enabled.is_active());
            let w = w.clone();
            spawn_bg(
                move || -> Result<Vec<String>, String> {
                    let mut said = Vec::new();
                    if let Some(want) = schedule {
                        if let (false, msg) = autoupdate::set_schedule(&want)? {
                            said.push(format!("Could not change the schedule: {msg}"));
                        }
                    }
                    if let Some(on) = timer {
                        // What set_enabled raises ends the close handler, logged.
                        if let (false, msg) = autoupdate::try_set_enabled(on)? {
                            said.push(format!("Could not change the timer: {msg}"));
                        }
                    }
                    Ok(said)
                },
                move |said| {
                    let said = match said {
                        Ok(said) => said,
                        Err(e) => {
                            eprintln!("almanac: {e}");
                            return;
                        }
                    };
                    let Some(w) = w.upgrade() else { return };
                    for message in said {
                        w.say(&message, 5);
                    }
                    let plugins_none = w.shared.borrow().plugins_none;
                    if plugins_none && w.content_stack.visible_child_name().as_deref() == Some("store") {
                        w.store_reload(true);
                    }
                },
            );
        });
        dialog.present(Some(&self.window));
        Ok(())
    }

    pub fn run_scheduled_now(&self) {
        let w = self.downgrade();
        tasks::run(
            &self.window,
            "Scheduled update",
            vec![tasks::Step::argv(
                "Running the scheduled update",
                vec![autoupdate::script_path().to_string_lossy().into_owned()],
            )],
            Some(Box::new(move |ok| {
                if let Some(w) = w.upgrade() {
                    w.after_pacman(ok);
                }
            })),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// This file's own source (`inspect.getsource` in the Python tests).
    fn source() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file!());
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// The non-test half of the source.
    fn body() -> String {
        let src = source();
        let cut = src.find("#[cfg(test)]").unwrap_or(src.len());
        src[..cut].to_string()
    }

    /// Demo mode shows fixtures: a demo screenshot once carried the real
    /// timer's next run. Nothing is read from systemd, no old unit is
    /// moved, and closing the dialog saves nothing.
    #[test]
    fn demo_mode_shows_fixtures_and_touches_no_unit() {
        let _env = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        crate::demo::TEST_DEMO.store(1, std::sync::atomic::Ordering::SeqCst);
        let read = super::Scheduled::read();
        crate::demo::TEST_DEMO.store(0, std::sync::atomic::Ordering::SeqCst);
        let s = read.unwrap();
        assert!(s.demo && s.omarchy && s.st.enabled);
        assert_eq!(s.st.next, super::Scheduled::demo().st.next);
        assert_eq!(s.legacy, crate::autoupdate::Legacy::None);
        assert_eq!(s.cfg, *crate::autoupdate::DEFAULTS);
        let src = body();
        let closed = &src[src.find("dialog.connect_closed").unwrap()..];
        assert!(closed[..300].contains("if demo {\n                return;"), "{}", &closed[..300]);
    }

    /// test_closing_preferences_reads_the_schedule_from_its_own_list:
    /// 1.4.0 reused one name for the schedule and the plugin scope, so
    /// closing the dialog saved a scope as the schedule.
    #[test]
    fn closing_preferences_reads_the_schedule_from_its_own_list() {
        let src = body();
        assert!(src.contains("let Some(&want) = sched_keys.get(combo.selected() as usize)"));
        // No bare "keys" left for the two lists to share.
        assert!(!src.lines().any(|l| l.trim_start().starts_with("let keys =")));
        assert!(!src.lines().any(|l| l.trim_start().starts_with("let keys:")));
    }

    /// test_spacing_comes_from_the_scale, on this section's code.
    #[test]
    fn spacing_comes_from_the_scale() {
        const SCALE: &[i64] = &[2, 4, 6, 8, 12, 16, 20, 24];
        let src = body();
        let re = regex_lite_spacing(&src);
        let offenders: BTreeMap<i64, Vec<String>> = re
            .into_iter()
            .filter(|(_p, v)| !SCALE.contains(v))
            .fold(BTreeMap::new(), |mut m, (p, v)| {
                m.entry(v).or_default().push(p);
                m
            });
        assert!(offenders.is_empty(), "spacing off the scale {SCALE:?}: {offenders:?}");
    }

    /// `(margin_start|…|spacing)(N)` / `set_…(N)` calls in Rust spelling.
    fn regex_lite_spacing(src: &str) -> Vec<(String, i64)> {
        const PROPS: &[&str] = &[
            "margin_start", "margin_end", "margin_top", "margin_bottom", "spacing",
            "child_spacing", "line_spacing",
        ];
        let mut out = Vec::new();
        for prop in PROPS {
            for pat in [format!(".{prop}("), format!(".set_{prop}(")] {
                let mut rest = src;
                while let Some(i) = rest.find(&pat) {
                    rest = &rest[i + pat.len()..];
                    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(v) = digits.parse::<i64>() {
                        out.push(((*prop).to_string(), v));
                    }
                }
            }
        }
        out
    }

    /// test_every_icon_name_is_in_adwaita_or_shipped_here, for the icons
    /// this section names: a theme-specific name renders as a blank square
    /// on every other theme.
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            eprintln!("skipped: Adwaita icon theme not installed");
            return;
        }
        let mut have = std::collections::BTreeSet::new();
        fn walk(d: &Path, have: &mut std::collections::BTreeSet<String>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p: PathBuf = e.path();
                if p.is_dir() {
                    walk(&p, have);
                } else if matches!(p.extension().and_then(|x| x.to_str()), Some("svg" | "png")) {
                    if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
                        have.insert(s.to_string());
                    }
                }
            }
        }
        walk(adwaita, &mut have);
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("icons"), &mut have);
        let src = body();
        let mut names = std::collections::BTreeSet::new();
        for piece in src.split('"').skip(1).step_by(2) {
            if piece.ends_with("-symbolic")
                && piece.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                names.insert(piece.to_string());
            }
        }
        assert!(!names.is_empty());
        // adw-* icons ship inside libadwaita itself.
        let missing: Vec<&String> =
            names.iter().filter(|n| !have.contains(*n) && !n.starts_with("adw-")).collect();
        assert!(missing.is_empty(), "missing icons: {missing:?}");
    }
}
