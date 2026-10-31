//! Install and remove beside the plugin rows: removal plans that show the
//! list before anything runs, orphan cleanup, installs.

use adw::prelude::*;
use gtk::gio;

use crate::i18n::tr;
use crate::ui::actions::Judge;
use crate::ui::shell::{source_label, spawn_bg_checked, AppItem, Win};
use crate::verify::{Outcome, ProbeValue};
use crate::{inventory, sources, tasks, verify};

use super::pyisms::{
    extra_str, popen_detached, py_oserror, py_splitlines, py_str_value, py_strip, truthy,
    uncaught_err,
};

/// Whether `open_url` hands `url` to the desktop: an http or https link to
/// a host, nothing else.
pub fn web_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
    })
}

/// The orphan split of `on_clean_orphans`: `(source, advice, name)` per row
/// -> (safe to remove, kept deliberately).
pub fn split_orphans<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
) -> (Vec<String>, Vec<String>) {
    let mut safe = Vec::new();
    let mut kept = Vec::new();
    for (source, advice, name) in rows {
        if source != inventory::ORPHAN {
            continue;
        }
        if advice == "keep" {
            &mut kept
        } else {
            &mut safe
        }
        .push(name.to_string());
    }
    (safe, kept)
}

/// What a removal asks before it runs, and the command it runs once the
/// user agrees -- the decision `on_remove` and `on_clean_orphans` make, with
/// `tasks.preview_removal` passed in (the seam the Python tests mock).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemovalPlan {
    pub title: String,
    pub body: String,
    pub detail: String,
    pub destructive: bool,
    pub argv: Vec<String>,
}

type Preview<'a> = &'a dyn Fn(&[String]) -> (bool, String);

/// `on_remove` for a package; `Err` is what it says instead of asking.
pub fn plan_package_removal(
    name: &str,
    preview_removal: Preview<'_>,
) -> Result<RemovalPlan, String> {
    let (ok, preview) = preview_removal(&[name.to_string()]);
    if !ok {
        return Err(format!("Cannot remove {name}: {preview}"));
    }
    let count = py_splitlines(&preview)
        .iter()
        .filter(|l| !py_strip(l).is_empty())
        .count();
    Ok(RemovalPlan {
        title: format!("Remove {name}?"),
        body: format!(
            "-Rns also takes dependencies nothing else needs. \
             {count} package(s) would be removed, listed below. \
             Configuration files are not kept."
        ),
        detail: preview,
        destructive: true,
        argv: vec![
            "pacman".into(),
            "-Rns".into(),
            "--noconfirm".into(),
            name.to_string(),
        ],
    })
}

/// `on_clean_orphans`; `Err` is what it says instead of asking.
pub fn plan_orphan_cleanup<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    preview_removal: Preview<'_>,
) -> Result<RemovalPlan, String> {
    let (safe, kept) = split_orphans(rows);

    if safe.is_empty() {
        let mut msg = "No orphans worth removing".to_string();
        if !kept.is_empty() {
            msg += &format!(
                " — {} kept deliberately (build tooling, debug packages)",
                kept.len()
            );
        }
        return Err(msg);
    }

    let (ok, preview) = preview_removal(&safe);
    if !ok {
        return Err(format!(
            "Could not work out what would be removed: {preview}"
        ));
    }
    let kept_note = if kept.is_empty() {
        String::new()
    } else {
        format!(
            "  {} kept deliberately (build tooling, debug packages)",
            kept.len()
        )
    };
    let mut argv: Vec<String> = vec!["pacman".into(), "-Rns".into(), "--noconfirm".into()];
    argv.extend(safe.iter().cloned());
    Ok(RemovalPlan {
        title: format!("Remove {} orphaned package(s)?", safe.len()),
        body: tr(
            "These are installed as dependencies but nothing needs them any \
                  more. Everything that would go is listed below.",
        ) + &kept_note,
        detail: preview,
        destructive: true,
        argv,
    })
}

/// `tasks.confirm`, as the removals reach it: the plan to show, and what to
/// do once the user agrees.
type Confirm<'a> = &'a dyn Fn(&RemovalPlan, Box<dyn Fn()>);

/// `tasks.run`, as the removals reach it: the task's title and steps.
type RunSteps = std::rc::Rc<dyn Fn(&str, Vec<tasks::Step>)>;

/// Ask with the plan; its command runs only from the confirmation.
fn ask_then_run(
    plan: RemovalPlan,
    title: String,
    label: String,
    confirm: Confirm<'_>,
    run: RunSteps,
) {
    let argv = plan.argv.clone();
    confirm(
        &plan,
        Box::new(move || run(&title, vec![tasks::Step::argv(label.clone(), argv.clone())])),
    );
}

/// `on_remove` for a package, with `tasks.preview_removal`, `tasks.confirm`,
/// `tasks.run` and `_say` passed in -- the seams the Python tests mock.
fn remove_package(
    name: &str,
    preview: Preview<'_>,
    say: &dyn Fn(&str),
    confirm: Confirm<'_>,
    run: RunSteps,
) {
    match plan_package_removal(name, preview) {
        Ok(plan) => ask_then_run(
            plan,
            format!("Removing {name}"),
            format!("Removing {name}"),
            confirm,
            run,
        ),
        Err(msg) => say(&msg),
    }
}

/// `on_clean_orphans`, with the same seams as [`remove_package`].
fn clean_orphans<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    preview: Preview<'_>,
    say: &dyn Fn(&str),
    confirm: Confirm<'_>,
    run: RunSteps,
) {
    match plan_orphan_cleanup(rows, preview) {
        Ok(plan) => ask_then_run(
            plan,
            "Removing orphans".to_string(),
            "Removing orphaned packages".to_string(),
            confirm,
            run,
        ),
        Err(msg) => say(&msg),
    }
}

impl Win {
    pub fn row_aur(&self, name: &str) {
        self.open_url(&format!("https://aur.archlinux.org/packages/{name}"));
    }

    pub fn row_remove(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            self.on_remove(&app);
        }
    }

    /// Hand a web page to the desktop's default browser.
    ///
    /// Only http and https: the links come from catalogues and caches (a
    /// store entry's repository, a plugin's page, an app's homepage), and the
    /// desktop's default handler for any other scheme can be a program --
    /// `file:///…/x.desktop`, `ssh:`, a custom URL handler. Almanac's own
    /// links to local folders and files go through `open_path`.
    pub fn open_url(&self, url: &str) {
        if !web_url(url) {
            self.say(&format!("Not opening {url}: only web links are opened"), 5);
            return;
        }
        if let Err(e) = gio::AppInfo::launch_default_for_uri(url, None::<&gio::AppLaunchContext>) {
            self.say(&format!("Could not open {url}: {}", e.message()), 5);
        }
    }

    /// Show a local folder or file Almanac itself points at (a checkout, a
    /// plugin's folder, its README) in the desktop's default app for it.
    pub fn open_path(&self, path: &std::path::Path) {
        let uri = gio::File::for_path(path).uri();
        if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
            self.say(
                &format!("Could not open {}: {}", path.display(), e.message()),
                5,
            );
        }
    }

    pub fn on_launch(&self, app: &inventory::App) {
        if self.demo_refuses() {
            return;
        }
        if let Err(e) = popen_detached(std::slice::from_ref(&app.exec_path)) {
            self.say(
                &format!(
                    "Could not launch {}: {}",
                    app.name,
                    py_oserror(&e, &app.exec_path)
                ),
                5,
            );
        }
    }

    /// Remove only the orphans judged safe, with pacman asking first.
    ///
    /// Build tooling and debug packages are excluded: the next AUR build pulls
    /// rust straight back, so removing it is churn rather than cleanup.
    pub fn on_clean_orphans(&self) {
        if self.demo_refuses() {
            return;
        }
        let store = self.shared.borrow().store.clone();
        let mut rows: Vec<(String, String, String)> = Vec::new();
        for i in 0..store.n_items() {
            let Some(obj) = store.item(i) else { continue };
            let row =
                AppItem::of(&obj).map(|it| (it.source.clone(), it.advice.clone(), it.name.clone()));
            rows.extend(row);
        }
        // What would go is asked of pacman off the main thread (see
        // `on_remove`); with nothing safe to remove there is nothing to ask.
        let (safe, _kept) = split_orphans(
            rows.iter()
                .map(|(s, a, n)| (s.as_str(), a.as_str(), n.as_str())),
        );
        if safe.is_empty() {
            clean_orphans(
                rows.iter()
                    .map(|(s, a, n)| (s.as_str(), a.as_str(), n.as_str())),
                &|_names| (false, String::new()),
                &|m| self.say(m, 5),
                &|plan, yes| self.confirm_removal(plan, yes),
                self.removal_runner(),
            );
            return;
        }
        let w = self.downgrade();
        self.busy(true);
        spawn_bg_checked(
            move || tasks::preview_removal(&safe),
            move |preview| {
                let Some(w) = w.upgrade() else { return };
                w.busy(false);
                let preview = preview.unwrap_or_else(|e| (false, e));
                clean_orphans(
                    rows.iter()
                        .map(|(s, a, n)| (s.as_str(), a.as_str(), n.as_str())),
                    &|_names| preview.clone(),
                    &|m| w.say(m, 5),
                    &|plan, yes| w.confirm_removal(plan, yes),
                    w.removal_runner(),
                );
            },
        );
    }

    /// `tasks.confirm` for a removal plan.
    fn confirm_removal(&self, plan: &RemovalPlan, yes: Box<dyn Fn()>) {
        tasks::confirm(
            &self.window,
            &plan.title,
            &plan.body,
            &plan.detail,
            &tr("Remove"),
            plan.destructive,
            yes,
        );
    }

    /// `tasks.run` for a removal, then `_after_pacman`.
    fn removal_runner(&self) -> RunSteps {
        let w = self.downgrade();
        std::rc::Rc::new(move |title: &str, steps: Vec<tasks::Step>| {
            let Some(w) = w.upgrade() else { return };
            let wt = w.downgrade();
            w.run_task(
                title,
                steps,
                Some(Box::new(move |ok| {
                    if let Some(w) = wt.upgrade() {
                        w.after_pacman(ok);
                    }
                })),
            );
        })
    }

    /// Install one result, through the tool that owns that kind of thing.
    pub fn on_install_item(&self, app: &inventory::App) {
        if self.demo_refuses() {
            return;
        }
        if app.source == inventory::PLUGIN {
            let url = extra_str(app, "repo");
            if url.is_empty() {
                return;
            }
            let w = self.downgrade();
            let nm = app.name.clone();
            let u = url.clone();
            tasks::confirm(
                &self.window,
                &format!("Install the {} plugin?", app.name),
                &tr(
                    "Omarchy shell plugins run inside the shell process with your \
                     permissions — there is no sandbox.",
                ),
                &format!("Source: {url}\n\n$ omarchy plugin add {url} --enable --yes"),
                &tr("Install"),
                false,
                move || {
                    let Some(w) = w.upgrade() else { return };
                    let wt = w.downgrade();
                    w.run_task(
                        &format!("Installing {nm}"),
                        vec![tasks::Step::argv(
                            format!("Installing {nm}"),
                            ["omarchy", "plugin", "add", &u, "--enable", "--yes"]
                                .iter()
                                .map(|s| s.to_string())
                                .collect(),
                        )],
                        Some(Box::new(move |ok| {
                            if let Some(w) = wt.upgrade() {
                                w.after_pacman(ok);
                            }
                        })),
                    );
                },
            );
            return;
        }

        // yay builds as this user and only needs root for the final pacman
        // call; --sudo pkexec is what lets that happen without a terminal.
        let argv = crate::tasks::yay_install(&[&app.name]);
        let w = self.downgrade();
        let nm = app.name.clone();
        let detail = format!(
            "{} {}\n{}\n\n$ {}",
            app.name,
            app.version,
            app.description,
            argv.join(" ")
        );
        tasks::confirm(
            &self.window,
            &format!("Install {}?", app.name),
            &format!(
                "From {}. AUR packages are built from source on this machine.",
                source_label(&app.source)
            ),
            &detail,
            &tr("Install"),
            false,
            move || {
                if let Some(w) = w.upgrade() {
                    w.install_package(&nm, argv.clone());
                }
            },
        );
    }

    /// Search and install, handed to yay in a terminal.
    ///
    /// yay's own interactive picker is better than anything reimplemented
    /// here, and it is the tool that owns the transaction.
    pub fn on_install(&self) {
        if self.demo_refuses() {
            return;
        }
        // Only the search box this page shows: a hidden one still holds
        // whatever another page left in it (the action is off there too;
        // a shortcut or a race must not get past it either).
        if !self.search_bar.is_visible() {
            return;
        }
        let text = py_strip(&self.search.text()).to_string();
        if text.is_empty() {
            self.search.grab_focus();
            self.say(&tr("Type what to search for, then press Install"), 5);
            return;
        }
        if sources::parse(&text).is_some() {
            self.on_install_link(&text);
            return;
        }
        let argv = crate::tasks::yay_install(&[&text]);
        let w = self.downgrade();
        let detail = format!("$ {}", argv.join(" "));
        let t = text.clone();
        tasks::confirm(
            &self.window,
            &format!("Install “{text}”?"),
            &tr(
                "This installs a package by exact name. To browse what matches \
                 first, use the App Store and search there.",
            ),
            &detail,
            &tr("Install"),
            false,
            move || {
                if let Some(w) = w.upgrade() {
                    w.install_package(&t, argv.clone());
                }
            },
        );
    }

    /// Install through yay: snapshot first, then check what changed.
    pub fn install_package(&self, name: &str, argv: Vec<String>) {
        if self.demo_refuses() {
            return;
        }
        let holder = match self.try_watch_defaults() {
            Ok(holder) => holder,
            Err(e) => return uncaught_err(&e),
        };

        let w = self.downgrade();
        let after: tasks::OnDone = Box::new(move |ok| {
            let Some(w) = w.upgrade() else { return };
            if ok {
                if let Err(e) = w.try_check_defaults(holder) {
                    return uncaught_err(&e);
                }
            }
            w.after_pacman(ok);
        });

        let nm = name.to_string();
        let judge: Judge = Box::new(move |b, a| {
            let a_s = match &a {
                Some(ProbeValue::Str(s)) => s.clone(),
                _ => String::new(),
            };
            let changed = truthy(&a) && a != b;
            Outcome::new(
                Some(changed),
                if changed {
                    format!("{nm} {a_s} installed")
                } else {
                    format!("{nm} was not installed")
                },
            )
        });
        // What the snapshot or the first probe raises ends this before the
        // task starts.
        let steps = match self.snapshot_first(
            vec![tasks::Step::argv(format!("Installing {name}"), argv)],
            &format!("installing {name}"),
        ) {
            Ok(steps) => steps,
            Err(e) => return uncaught_err(&e),
        };
        let w = self.downgrade();
        let title = format!("Installing {name}");
        self.checked(
            Some(verify::installed_package_try(name)),
            judge,
            Some(after),
            uncaught_err,
            move |on_done| {
                if let Some(w) = w.upgrade() {
                    w.run_task(&title, steps, Some(on_done));
                }
            },
        );
    }

    /// Remove a package, with its orphaned dependencies.
    ///
    /// -Rns is what a user means by "uninstall", but it can take more than
    /// expected, so the terminal shows the full list before confirming --
    /// no --noconfirm here, deliberately.
    pub fn on_remove(&self, app: &inventory::App) {
        if self.demo_refuses() {
            return;
        }
        if app.source == inventory::APPIMAGE {
            // The file and its rollback copy are deleted outright, so the
            // row menu's "Remove…" asks first, as the App Store does.
            let path = std::path::PathBuf::from(&app.exec_path);
            let detail = format!(
                "{}\n{}",
                path.display(),
                path.with_extension("previous").display()
            );
            let (w, app) = (self.downgrade(), app.clone());
            tasks::confirm(
                &self.window,
                &format!("Remove {}?", app.name),
                &tr(
                    "The AppImage, its rollback copy and its launcher entry are \
                     deleted. Documents you created with it are untouched.",
                ),
                &detail,
                &tr("Remove"),
                true,
                move || {
                    if let Some(win) = w.upgrade() {
                        win.remove_appimage(&app);
                    }
                },
            );
            return;
        }
        if app.source == inventory::FLATPAK {
            // `app.extra.get("appid", app.name)`: whatever is stored, as
            // str() prints it; the name only when the key is missing.
            let appid = app
                .extra
                .get("appid")
                .map(py_str_value)
                .unwrap_or_else(|| app.name.clone());
            let w = self.downgrade();
            let nm = app.name.clone();
            let id = appid.clone();
            tasks::confirm(
                &self.window,
                &format!("Remove {}?", app.name),
                &tr("The Flatpak and its data are removed."),
                &format!("$ flatpak uninstall --assumeyes {appid}"),
                &tr("Remove"),
                true,
                move || {
                    let Some(w) = w.upgrade() else { return };
                    let wt = w.downgrade();
                    w.run_task(
                        &format!("Removing {nm}"),
                        vec![tasks::Step::argv(
                            format!("Removing {nm}"),
                            vec![
                                "flatpak".into(),
                                "uninstall".into(),
                                "--assumeyes".into(),
                                id.clone(),
                            ],
                        )],
                        Some(Box::new(move |ok| {
                            if let Some(w) = wt.upgrade() {
                                w.after_pacman(ok);
                            }
                        })),
                    );
                },
            );
            return;
        }

        // `pacman -Rs --print` can take a while (60 s at worst): it runs off
        // the main thread, and the plan is made from its answer once it is in.
        let name = app.name.clone();
        let names = vec![name.clone()];
        let w = self.downgrade();
        self.busy(true);
        spawn_bg_checked(
            move || tasks::preview_removal(&names),
            move |preview| {
                let Some(w) = w.upgrade() else { return };
                w.busy(false);
                let preview = preview.unwrap_or_else(|e| (false, e));
                remove_package(
                    &name,
                    &|_names| preview.clone(),
                    &|m| w.say(m, 5),
                    &|plan, yes| w.confirm_removal(plan, yes),
                    w.removal_runner(),
                );
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const SRC: &str = include_str!("removal.rs");

    /// `pacman -Rs --print` waits up to 60 s: the window asks it from a
    /// background thread, never from its own.
    #[test]
    fn the_removal_preview_runs_off_the_main_thread() {
        let code = SRC.split("#[cfg(test)]").next().unwrap_or(SRC);
        for f in ["pub fn on_remove(", "pub fn on_clean_orphans("] {
            let body = code
                .split(f)
                .nth(1)
                .and_then(|s| s.split("\n    }\n").next())
                .unwrap_or("");
            assert!(!body.is_empty(), "{f} not found");
            assert!(
                !body.contains("&tasks::preview_removal"),
                "{f} previews on the main thread"
            );
            assert!(
                body.contains("spawn_bg_checked(\n            move || tasks::preview_removal("),
                "{f}"
            );
        }
    }

    /// Catalogue values reach `open_url`; only web links get through.
    #[test]
    fn only_web_links_are_opened() {
        for ok in [
            "https://github.com/a/b",
            "http://example.com",
            "HTTPS://Example.com/x?y=1#z",
        ] {
            assert!(web_url(ok), "{ok}");
        }
        for bad in [
            "",
            "repo",
            "/home/me/repo",
            "file:///home/me/repo",
            "file:///usr/share/applications/x.desktop",
            "ext::sh -c x",
            "javascript:alert(1)",
            "ssh://host/x",
            "steam://run/1",
            "https://",
            "smb://server/share",
            "data:text/html,x",
        ] {
            assert!(!web_url(bad), "{bad}");
        }
        // Almanac's own folder links go through open_path, not open_url.
        for (file, src) in [
            ("actions/rows.rs", include_str!("../actions/rows.rs")),
            ("plugins/page.rs", include_str!("page.rs")),
            ("store_actions.rs", include_str!("../store_actions.rs")),
        ] {
            assert!(
                !src.contains("open_url(&format!(\"file://"),
                "{file} sends a file:// link to open_url"
            );
            assert!(
                !src.contains("open_url(&gtk::gio::File"),
                "{file} sends a file:// link to open_url"
            );
        }
    }

    // RemovalCommands.test_package_removal_shows_the_list_before_removing_anything
    #[test]
    fn package_removal_shows_the_list_before_removing_anything() {
        let asked = Mutex::new(Vec::new());
        let preview = |names: &[String]| {
            asked.lock().unwrap().push(names.to_vec());
            (true, "somepkg 1\nleftover-dep 2".to_string())
        };
        let plan = plan_package_removal("somepkg", &preview).expect("removal must ask first");
        // The preview is the whole point: -Rns takes dependencies too, and
        // the user has to be able to see which ones.
        assert!(plan.detail.contains("leftover-dep"));
        assert!(plan.destructive);
        assert!(plan.body.contains("2 package(s) would be removed"));
        assert_eq!(plan.argv.join(" "), "pacman -Rns --noconfirm somepkg");
        assert_eq!(*asked.lock().unwrap(), vec![vec!["somepkg".to_string()]]);
    }

    /// `_Confirmed`: stands in for tasks.confirm/tasks.run.
    #[derive(Default)]
    struct Confirmed {
        confirm_args: std::cell::RefCell<Option<RemovalPlan>>,
        on_yes: std::cell::RefCell<Option<Box<dyn Fn()>>>,
        ran: std::rc::Rc<std::cell::RefCell<Vec<tasks::Step>>>,
    }

    impl Confirmed {
        fn confirm(&self, plan: &RemovalPlan, yes: Box<dyn Fn()>) {
            *self.confirm_args.borrow_mut() = Some(plan.clone());
            *self.on_yes.borrow_mut() = Some(yes);
        }
        fn run(&self) -> RunSteps {
            let ran = self.ran.clone();
            std::rc::Rc::new(move |_title: &str, steps: Vec<tasks::Step>| {
                ran.borrow_mut().extend(steps)
            })
        }
        fn say_yes(&self) {
            let yes = self
                .on_yes
                .borrow_mut()
                .take()
                .expect("nothing asked for confirmation");
            yes();
        }
        fn commands(&self) -> Vec<String> {
            self.ran
                .borrow()
                .iter()
                .map(|s| match &s.action {
                    tasks::StepAction::Argv(argv) => argv.join(" "),
                    _ => "<callable>".to_string(),
                })
                .collect()
        }
    }

    // The same test, through `remove_package`, the body of `on_remove`.
    #[test]
    fn on_remove_runs_nothing_before_the_user_confirms() {
        let fake = Confirmed::default();
        let said = std::cell::RefCell::new(Vec::new());
        let preview = |_n: &[String]| (true, "somepkg 1\nleftover-dep 2".to_string());
        remove_package(
            "somepkg",
            &preview,
            &|m| said.borrow_mut().push(m.to_string()),
            &|p, y| fake.confirm(p, y),
            fake.run(),
        );
        let args = fake
            .confirm_args
            .borrow()
            .clone()
            .expect("removal must ask first");
        assert!(
            fake.ran.borrow().is_empty(),
            "nothing may run before the user confirms"
        );
        assert!(args.detail.contains("leftover-dep"));
        assert!(args.destructive);
        fake.say_yes();
        assert!(fake
            .commands()
            .contains(&"pacman -Rns --noconfirm somepkg".to_string()));
        assert!(said.borrow().is_empty());
    }

    #[test]
    fn on_remove_asks_nothing_when_the_preview_fails() {
        let fake = Confirmed::default();
        let said = std::cell::RefCell::new(Vec::new());
        let preview = |_n: &[String]| (false, "target not found".to_string());
        remove_package(
            "somepkg",
            &preview,
            &|m| said.borrow_mut().push(m.to_string()),
            &|p, y| fake.confirm(p, y),
            fake.run(),
        );
        assert!(
            fake.confirm_args.borrow().is_none(),
            "must not offer to remove what it cannot describe"
        );
        assert!(fake.ran.borrow().is_empty());
        assert_eq!(
            *said.borrow(),
            vec!["Cannot remove somepkg: target not found".to_string()]
        );
    }

    #[test]
    fn on_clean_orphans_runs_only_the_safe_ones_after_yes() {
        let fake = Confirmed::default();
        let asked = Mutex::new(Vec::new());
        let preview = |names: &[String]| {
            asked.lock().unwrap().push(names.to_vec());
            (true, "libfoo 1".to_string())
        };
        clean_orphans(
            [
                (inventory::ORPHAN, "keep", "rust"),
                (inventory::ORPHAN, "remove", "libfoo"),
            ],
            &preview,
            &|_m| panic!("must ask"),
            &|p, y| fake.confirm(p, y),
            fake.run(),
        );
        assert_eq!(*asked.lock().unwrap(), vec![vec!["libfoo".to_string()]]);
        assert!(fake.ran.borrow().is_empty());
        fake.say_yes();
        let joined = fake.commands().join(" ");
        assert!(joined.contains("libfoo"));
        assert!(
            !joined.contains("rust"),
            "rust is build tooling and must be kept"
        );
        assert_eq!(fake.ran.borrow()[0].label, "Removing orphaned packages");
    }

    // RemovalCommands.test_removal_is_abandoned_when_the_preview_fails
    #[test]
    fn removal_is_abandoned_when_the_preview_fails() {
        let preview = |_n: &[String]| (false, "target not found".to_string());
        // Err: nothing to confirm, so nothing can run.
        assert_eq!(
            plan_package_removal("somepkg", &preview),
            Err("Cannot remove somepkg: target not found".to_string()),
            "must not offer to remove what it cannot describe"
        );
    }

    // RemovalCommands.test_orphan_cleanup_excludes_the_keep_list
    #[test]
    fn orphan_cleanup_excludes_the_keep_list() {
        let asked = Mutex::new(Vec::new());
        let preview = |names: &[String]| {
            asked.lock().unwrap().push(names.to_vec());
            (true, "libfoo 1".to_string())
        };
        let plan = plan_orphan_cleanup(
            [
                (inventory::ORPHAN, "keep", "rust"),
                (inventory::ORPHAN, "remove", "libfoo"),
            ],
            &preview,
        )
        .expect("asks first");
        // rust must not even reach the preview, let alone the removal.
        assert_eq!(*asked.lock().unwrap(), vec![vec!["libfoo".to_string()]]);
        let joined = plan.argv.join(" ");
        assert!(joined.contains("libfoo"));
        assert!(!joined.contains("rust"));
    }

    #[test]
    fn split_orphans_ignores_other_sources() {
        let (safe, kept) = split_orphans([
            (inventory::ORPHAN, "keep", "rust"),
            (inventory::ORPHAN, "remove", "libfoo"),
            (inventory::REPO, "", "firefox"),
        ]);
        assert_eq!(safe, vec!["libfoo".to_string()]);
        assert_eq!(kept, vec!["rust".to_string()]);
    }
}
