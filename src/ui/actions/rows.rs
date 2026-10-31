//! Row actions: visit, release notes, folder, changes, pull, stash, and
//! self-update and About.

use adw::prelude::*;
use serde_json::Value;

use crate::i18n::tr;
use crate::tasks::Step;
use crate::ui::shell::{spawn_bg, Win};
use crate::{ghcache, selfupdate, tasks, theming};

use super::pyisms::{not_a_path, py_str, py_type, strs, truthy};
use super::raised;

/// `github.com/owner/repo[/...]` -> `https://github.com/owner/repo/releases`;
/// any other non-empty URL is returned as is, an empty one as `None`.
/// (The URL logic of `_row_notes`.)
pub fn releases_url(url: &str) -> Option<String> {
    let tail = match url.split_once("github.com/") {
        Some((_, t)) => t,
        None => url,
    };
    let parts: Vec<&str> = tail.trim_matches('/').split('/').collect();
    if parts.len() >= 2 {
        Some(format!(
            "https://github.com/{}/{}/releases",
            parts[0], parts[1]
        ))
    } else if !url.is_empty() {
        Some(url.to_string())
    } else {
        None
    }
}

impl Win {
    pub fn row_visit(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            if let Some(url) = app.extra.get("url").filter(|v| truthy(v)) {
                self.open_url_value(url);
            }
        }
    }

    /// `self.open_url(app.extra["url"])` with the raw value: PyGObject
    /// refuses anything but a str.
    pub(super) fn open_url_value(&self, url: &Value) {
        match url {
            Value::String(s) => self.open_url(s),
            other => raised(&format!(
                "TypeError: Must be string, not {}",
                py_type(other)
            )),
        }
    }

    /// Jump to the releases page, where the real changelog is.
    pub fn row_notes(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        let Some(app) = app else {
            return;
        };
        let url = match app.extra.get("url") {
            None => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => {
                raised(&format!(
                    "AttributeError: '{}' object has no attribute 'split'",
                    py_type(other)
                ));
                return;
            }
        };
        // github.com/owner/repo[/...] -> github.com/owner/repo/releases
        if let Some(target) = releases_url(&url) {
            self.open_url(&target);
        }
    }

    /// Update Almanac, then offer to restart into the new code.
    ///
    /// The check is a `git fetch` (up to 90 s): it runs off the main thread,
    /// and the question is asked when it answers.
    pub fn on_self_update(&self) {
        if self.demo_refuses() {
            return;
        }
        let w = self.downgrade();
        spawn_bg(
            || selfupdate::try_check(true),
            move |info| match info {
                Ok(info) => {
                    if let Some(w) = w.upgrade() {
                        w.offer_self_update(info);
                    }
                }
                Err(e) => raised(&e),
            },
        );
    }

    fn offer_self_update(&self, info: selfupdate::Info) {
        if selfupdate::offers_download(&info) {
            // A newer release of the package downloaded from GitHub: nothing
            // else updates it, so Almanac downloads, checks and installs it.
            let steps = selfupdate::package::update_steps(&info.latest);
            if steps.is_empty() {
                self.say(&info.note, 8);
                return;
            }
            let w = self.downgrade();
            let steps = std::cell::RefCell::new(Some(steps));
            tasks::confirm(
                &self.window,
                &format!("Update Almanac to {}?", info.latest),
                &format!(
                    "This is {}. The package is downloaded from the v{} release, checked \
                     against the SHA256SUMS published with it, and installed with \
                     pacman -U, which asks for your password.",
                    info.version, info.latest
                ),
                &format!(
                    "{}\n{}\n\n$ pkexec pacman -U {}",
                    selfupdate::package::asset_url(&info.latest, selfupdate::package::ASSET),
                    selfupdate::package::asset_url(&info.latest, selfupdate::package::SUMS),
                    selfupdate::package::package_path(&info.latest).display()
                ),
                &tr("Update"),
                false,
                move || {
                    let (Some(win), Some(steps)) = (w.upgrade(), steps.borrow_mut().take()) else {
                        return;
                    };
                    let w2 = win.downgrade();
                    win.run_task(
                        "Updating Almanac",
                        steps,
                        Some(Box::new(move |ok| {
                            if let Some(win) = w2.upgrade() {
                                win.after_self_update(ok);
                            }
                        })),
                    );
                },
            );
            return;
        }
        if !info.can_update {
            let note = if info.note.is_empty() {
                "Almanac is up to date".to_string()
            } else {
                info.note
            };
            self.say(&note, 8);
            return;
        }
        let w = self.downgrade();
        tasks::confirm(
            &self.window,
            "Update Almanac?",
            &format!(
                "{} commit(s) behind. The update is a fast-forward pull, so it cannot \
                 overwrite local work — if there were any, this would refuse instead.",
                info.behind
            ),
            &format!(
                "Checkout  {}\nRemote    {}\nNewest    {}\n\n$ git pull --ff-only",
                selfupdate::ROOT.display(),
                info.remote,
                info.latest
            ),
            &tr("Update"),
            false,
            move || {
                let Some(win) = w.upgrade() else { return };
                let steps = selfupdate::update_steps()
                    .into_iter()
                    .map(|(label, argv)| Step::argv(label, argv))
                    .collect();
                let w2 = win.downgrade();
                win.run_task(
                    "Updating Almanac",
                    steps,
                    Some(Box::new(move |ok| {
                        if let Some(win) = w2.upgrade() {
                            win.after_self_update(ok);
                        }
                    })),
                );
            },
        );
    }

    pub fn after_self_update(&self, ok: bool) {
        if !ok {
            self.say(&tr("Almanac was not updated"), 8);
            return;
        }
        // The running process cannot adopt the new source, so say so rather
        // than letting the version in the list disagree with what is running.
        let toast = adw::Toast::builder()
            .title(tr("Almanac updated — restart to use the new version"))
            .timeout(0)
            .build();
        toast.set_button_label(Some("Restart"));
        toast.connect_button_clicked(|_| selfupdate::restart());
        self.toasts.add_toast(toast);
        self.reload();
    }

    /// The About dialog. What it shows takes a handful of git calls and the
    /// theme and GitHub summaries: gathered off the main thread, then shown.
    pub fn on_about(&self) {
        let w = self.downgrade();
        spawn_bg(
            || -> Result<_, String> {
                // What raises in here ends on_about before the dialog is
                // presented, in the Python's order.
                let info = selfupdate::try_check(false)?;
                let theme = theming::try_describe()?;
                let github = ghcache::try_describe()?;
                let notes = selfupdate::try_release_notes()?;
                Ok((info, theme, github, notes))
            },
            move |gathered| match gathered {
                Ok((info, theme, github, notes)) => {
                    if let Some(w) = w.upgrade() {
                        w.show_about(info, &theme, &github, notes);
                    }
                }
                Err(e) => raised(&e),
            },
        );
    }

    fn show_about(
        &self,
        info: selfupdate::Info,
        theme: &str,
        github: &str,
        notes: Option<(String, String)>,
    ) {
        let about = adw::AboutDialog::builder()
            .application_name("Almanac")
            .application_icon("io.github.twodxme.Almanac")
            .version(info.version.as_str())
            .developer_name("twodxme")
            .comments(tr(
                "A catalogue of everything installed on this machine: where each thing \
                 came from, how old it is, whether it still has a maintainer, and what \
                 needs attention.",
            ))
            .website("https://github.com/twodxme/almanac")
            .issue_url("https://github.com/twodxme/almanac/issues")
            .license_type(gtk::License::MitX11)
            .build();
        about.add_credit_section(
            Some("Catalogues"),
            &[
                "Omarchy plugin marketplace https://plugins.omarchy.org",
                "ArtCraft https://getartcraft.com",
            ],
        );
        about.set_debug_info(&format!(
            "Installed as: {}\n{}\n{}\n{}",
            info.kind, info.note, theme, github
        ));
        if let Some((version, markup)) = notes {
            about.set_release_notes_version(&version);
            about.set_release_notes(&markup);
        }
        about.present(Some(&self.window));
    }

    pub fn row_folder(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            if let Some(repo) = app.extra.get("repo").filter(|v| truthy(v)) {
                self.open_path(std::path::Path::new(&py_str(repo)));
            }
        }
    }

    /// Show what is uncommitted, read-only, before offering to move it.
    pub fn row_changes(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        let repo = match app
            .as_ref()
            .and_then(|a| a.extra.get("repo"))
            .filter(|v| truthy(v))
        {
            None => return,
            Some(Value::String(r)) => r.clone(),
            Some(other) => return raised(&not_a_path(other)),
        };
        self.run_task(
            &format!("Local changes in {name}"),
            vec![
                Step::argv(
                    "Working tree",
                    strs(&["git", "-C", &repo, "status", "--short"]),
                ),
                Step::argv(
                    "Diff summary",
                    strs(&["git", "-C", &repo, "diff", "--stat"]),
                ),
            ],
            None,
        );
    }

    pub fn row_pull(&self, name: &str) {
        if self.demo_refuses() {
            return;
        }
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            self.on_git_pull(&app);
        }
    }

    /// Update a checkout that has local edits -- see on_git_pull.
    pub fn row_stash(&self, name: &str) {
        if self.demo_refuses() {
            return;
        }
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            self.on_git_pull(&app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_url_from_github() {
        assert_eq!(
            releases_url("https://github.com/a/b/tree/main").as_deref(),
            Some("https://github.com/a/b/releases")
        );
        assert_eq!(releases_url("example.com").as_deref(), Some("example.com"));
        assert_eq!(releases_url(""), None);
    }
}
