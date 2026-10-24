//! Plugin update planning: what a bulk update would do, the steps, and its
//! confirmation.

use std::collections::BTreeMap;

use crate::i18n::tr;
use crate::{plugins as plugins_mod, store as store_mod, tasks, verify};
use crate::ui::actions::Judge;
use crate::ui::shell::{Win, spawn_bg};
use crate::verify::{Outcome, ProbeValue, TryProbe};

use super::pyisms::{py_repr, py_splitlines, run_checked, uncaught, uncaught_err};

/// What `_confirm_plugin_updates` decides before it asks: the plugins to
/// update, those held back by the scope, those that need the user, and the
/// lines of the confirmation's detail.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpdatePlan {
    pub ready: Vec<plugins_mod::State>,
    pub held: Vec<plugins_mod::State>,
    /// Why each of `held` is held, in the same order.
    pub held_why: Vec<String>,
    /// id -> the reviewed commit a ready plugin may update to, under
    /// "Verified only" (its step checks it again; see `plugin_step`).
    pub pins: BTreeMap<String, String>,
    pub blocked: Vec<plugins_mod::State>,
    /// `None` for a scope `SCOPES` does not list: the Python's
    /// `dict(...)[scope]` raises KeyError there, once there is something to
    /// update.
    pub scope_label: Option<String>,
    pub lines: Vec<String>,
    /// What `s.describe()` raised building the "needs you" lines (the
    /// Python raises there, after the scope label is looked up).
    pub raised: Option<String>,
}

/// Every installed plugin classified (fetching), with its listing: what a
/// bulk update plans from. `have` is the catalog already loaded, if any.
pub(in crate::ui) fn plugin_update_states(
    have: Vec<crate::store::Plugin>,
) -> Result<(Vec<plugins_mod::State>, BTreeMap<String, plugins_mod::Listing>), String> {
    let plugins = if have.is_empty() { store_mod::try_catalog(false)? } else { have };
    let mut trust: BTreeMap<String, plugins_mod::Listing> = BTreeMap::new();
    for p in &plugins {
        trust.insert(p.id.clone(), p.try_listing()?);
    }
    let mut cache = plugins_mod::FetchCache::new();
    let mut states = Vec::new();
    let pdir = plugins_mod::plugins_dir();
    if pdir.is_dir() {
        let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&pdir)
            .map(|rd| {
                // pathlib's glob("*") matches hidden entries too.
                rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()
            })
            .unwrap_or_default();
        dirs.sort();
        for d in dirs {
            if d.join("manifest.json").is_file() {
                states.push(plugins_mod::try_classify(&d, Some(&mut cache), true)?);
            }
        }
    }
    Ok((states, trust))
}

/// One plugin's update, as a task step. `pin`: the commit the plan let it
/// update to, under "Verified only" (see `plugins::pin_hold`); upstream
/// moving past it between the check and the run holds it back, and an
/// update that lands anywhere else is reported as a failure.
pub(in crate::ui) fn plugin_step(pid: String, pin: Option<String>) -> tasks::StepFn {
    std::sync::Arc::new(move |log: tasks::LogFn<'_>| {
        // Checked again now: the dialog may have stayed open while the
        // plugin was edited, or swapped for a development link.
        // What classify raises fails the step with str(exc).
        let now = plugins_mod::try_classify(
            &plugins_mod::plugins_dir().join(&pid),
            Some(&mut plugins_mod::FetchCache::new()),
            true,
        )
        .map_err(|e| crate::ui::shell::exc_str(&e))?;
        if !now.updatable() {
            let why = now.try_describe().map_err(|e| crate::ui::shell::exc_str(&e))?;
            log(&format!("{pid}: skipped — {why}"));
            return Err(format!("{pid} changed since the check"));
        }
        if let Some(pin) = &pin {
            if !now.remote.eq_ignore_ascii_case(pin) {
                log(&format!("{pid}: skipped — {}", tr("changed since it was reviewed")));
                return Err(format!("{pid} changed since the check"));
            }
        }
        let r = run_checked(&plugins_mod::update_argv(&pid), 300)?;
        let both = r.stdout.clone() + &r.stderr;
        let lines: Vec<&str> = py_splitlines(&both);
        // The command's own words, shown as it printed them.
        for line in &lines[lines.len().saturating_sub(6)..] {
            log(&tasks::output(line));
        }
        if !r.ok() {
            return Err(format!("{pid} was not updated"));
        }
        if let Some(pin) = &pin {
            // `omarchy plugin update` pulls whatever upstream has at that
            // moment; it cannot be told which commit. Say so if that was not
            // the one reviewed.
            let head = plugins_mod::checkout_head(&plugins_mod::plugins_dir().join(&pid));
            if !head.eq_ignore_ascii_case(pin) {
                log(&format!("{pid}: {}", tr("updated past the reviewed commit — upstream changed during the update")));
                return Err(format!("{pid} is not at the reviewed commit"));
            }
        }
        Ok(())
    })
}

/// Why a plugin is held back from a bulk update in this scope, or None.
/// Its listing counts only when the checkout's origin is the repository the
/// listing names -- a folder that only shares a listed id is unlisted -- and
/// under "Verified only" upstream must still be at the reviewed commit.
fn held_reason(s: &plugins_mod::State, listing: Option<&plugins_mod::Listing>, scope: &str) -> Option<String> {
    let key = plugins_mod::listed_trust(&s.url, listing);
    if !plugins_mod::in_scope(key, scope) {
        return Some(match (listing, key) {
            (Some(_), None) => tr("not the listed repository"),
            (_, Some(k)) if !k.is_empty() => k.to_string(),
            _ => "not listed".to_string(),
        });
    }
    plugins_mod::pin_hold(s, listing.filter(|_| key.is_some()), scope)
}

/// The planning half of `_confirm_plugin_updates`. `trust` maps plugin id
/// to its marketplace listing.
pub fn plan_plugin_updates(
    states: &[plugins_mod::State],
    trust: &BTreeMap<String, plugins_mod::Listing>,
    scope: &str,
) -> UpdatePlan {
    let mut ready: Vec<plugins_mod::State> = Vec::new();
    let mut held: Vec<plugins_mod::State> = Vec::new();
    let mut held_why: Vec<String> = Vec::new();
    let mut pins: BTreeMap<String, String> = BTreeMap::new();
    for s in states.iter().filter(|s| s.updatable()) {
        let listing = trust.get(&s.id);
        match held_reason(s, listing, scope) {
            None => {
                if scope == "verified" && listing.is_some_and(|l| !l.validated_commit.trim().is_empty()) {
                    pins.insert(s.id.clone(), s.remote.clone());
                }
                ready.push(s.clone());
            }
            Some(why) => {
                held.push(s.clone());
                held_why.push(why);
            }
        }
    }
    let blocked: Vec<plugins_mod::State> = states
        .iter()
        .filter(|s| {
            (s.kind == plugins_mod::LOCAL_CHANGES || s.kind == plugins_mod::ERROR) && s.behind != 0
        })
        .cloned()
        .collect();
    let scope_label = plugins_mod::SCOPES
        .iter()
        .find(|(k, _l, _s)| *k == scope)
        .map(|(_k, l, _s)| (*l).to_string());
    let label = scope_label.as_deref().unwrap_or("");
    let mut lines: Vec<String> =
        ready.iter().map(|s| format!("{:<40} {} commit(s)", s.id, s.behind)).collect();
    if !held.is_empty() {
        lines.push(String::new());
        lines.push(format!("Held back (outside “{label}”):"));
        for (s, why) in held.iter().zip(&held_why) {
            lines.push(format!("  {} — {why}", s.id));
        }
    }
    let mut raised = None;
    if !blocked.is_empty() {
        lines.push(String::new());
        lines.push("Not updated (needs you):".to_string());
        for s in &blocked {
            match s.try_describe() {
                Ok(d) => lines.push(format!("  {} — {d}", s.id)),
                Err(e) => {
                    raised = Some(e);
                    break;
                }
            }
        }
    }
    UpdatePlan { ready, held, held_why, pins, blocked, scope_label, lines, raised }
}

impl Win {
    /// Update every plugin with a clean update waiting, within the scope
    /// chosen in Preferences; say which were held back and why.
    pub fn on_update_plugins(&self) {
        let have = self.shared.borrow().plugins.clone();
        // What raises in here ends the Python's worker: nothing is asked.
        let work = move || plugin_update_states(std::sync::Arc::unwrap_or_clone(have));
        self.say(&tr("Checking plugins…"), 5);
        let w = self.downgrade();
        spawn_bg(work, move |res| match res {
            Ok((states, trust)) => {
                if let Some(w) = w.upgrade() {
                    w.confirm_plugin_updates(states, trust);
                }
            }
            Err(e) => uncaught_err(&e),
        });
    }

    pub fn confirm_plugin_updates(
        &self,
        states: Vec<plugins_mod::State>,
        trust: BTreeMap<String, plugins_mod::Listing>,
    ) {
        let scope = match plugins_mod::try_scope() {
            Ok(scope) => scope,
            Err(e) => return uncaught_err(&e),
        };
        let plan = plan_plugin_updates(&states, &trust, &scope);
        if plan.ready.is_empty() {
            let held = if plan.held.is_empty() {
                String::new()
            } else {
                format!(" — {} held back by the update scope", plan.held.len())
            };
            self.say(&(tr("No plugin updates to install") + &held), 8);
            return;
        }
        let Some(scope_label) = plan.scope_label.clone() else {
            // `dict(...)[scope]` raises in the idle callback: logged, nothing
            // asked.
            uncaught("KeyError", &py_repr(&scope));
            return;
        };
        if let Some(e) = &plan.raised {
            return uncaught_err(e);
        }
        let ready = plan.ready.clone();
        let pins = plan.pins.clone();


        let heads: Vec<(String, String)> = ready
            .iter()
            .map(|s| (s.id.clone(), plugins_mod::plugins_dir().join(&s.id).display().to_string()))
            .collect();

        let w = self.downgrade();
        tasks::confirm(
            &self.window,
            &format!("Update {} plugin(s)?", ready.len()),
            &format!(
                "Scope: {scope_label} (change it in Preferences). Each goes \
                 through `omarchy plugin update`, which validates it and rolls \
                 back one that fails."
            ),
            &plan.lines.join("\n"),
            &tr("Update"),
            false,
            move || {
                let Some(w) = w.upgrade() else { return };
                let probe_heads = heads.clone();
                let probe: TryProbe = std::sync::Arc::new(move || {
                    let mut out = BTreeMap::new();
                    for (pid, path) in &probe_heads {
                        let head = match verify::git_head_try(path)()? {
                            Some(ProbeValue::Str(s)) => Some(s),
                            _ => None,
                        };
                        out.insert(pid.clone(), head);
                    }
                    Ok(Some(ProbeValue::Map(out)))
                });
                let judge_heads = heads.clone();
                let judge: Judge = Box::new(move |before, after| {
                    let as_map = |v: Option<ProbeValue>| match v {
                        Some(ProbeValue::Map(m)) => m,
                        _ => BTreeMap::new(),
                    };
                    let (before, after) = (as_map(before), as_map(after));
                    let moved: Vec<&String> = judge_heads
                        .iter()
                        .map(|(p, _)| p)
                        .filter(|p| before.get(*p) != after.get(*p))
                        .collect();
                    let stuck: Vec<&str> = judge_heads
                        .iter()
                        .map(|(p, _)| p)
                        .filter(|p| !moved.contains(p))
                        .map(String::as_str)
                        .collect();
                    Outcome::new(
                        Some(!moved.is_empty() && stuck.is_empty()),
                        format!("{} plugin(s) updated", moved.len())
                            + &if stuck.is_empty() {
                                String::new()
                            } else {
                                format!("; not updated: {}", stuck.join(", "))
                            },
                    )
                });
                let wt = w.downgrade();
                let then: tasks::OnDone = Box::new(move |ok| {
                    if let Some(w) = wt.upgrade() {
                        w.after_pacman(ok);
                    }
                });
                let steps: Vec<tasks::Step> = ready
                    .iter()
                    .map(|s| tasks::Step::call(format!("Updating {}", s.id), plugin_step(s.id.clone(), pins.get(&s.id).cloned())))
                    .collect();
                let wr = w.downgrade();
                w.checked(Some(probe), judge, Some(then), uncaught_err, move |on_done| {
                    if let Some(w) = wr.upgrade() {
                        tasks::run(&w.window, &tr("Updating plugins"), steps, Some(on_done));
                    }
                });
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_plan_holds_back_out_of_scope_and_lists_blocked() {
        let states = vec![
            plugins_mod::State::new("a", "update", "", 2, "https://github.com/o/a"),
            plugins_mod::State::new("b", "update", "", 1, ""),
            plugins_mod::State::new("c", "local-changes", "modified", 3, ""),
            plugins_mod::State::new("d", "current", "", 0, ""),
        ];
        let trust: BTreeMap<String, plugins_mod::Listing> = [(
            "a".to_string(),
            plugins_mod::Listing { key: "official".into(), repo: "https://github.com/o/a".into(), ..Default::default() },
        )]
        .into();
        let plan = plan_plugin_updates(&states, &trust, "verified");
        assert_eq!(plan.ready.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["a"]);
        assert_eq!(plan.held.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b"]);
        assert_eq!(plan.blocked.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["c"]);
        assert_eq!(plan.scope_label.as_deref(), Some("Verified only"));
        assert_eq!(plan.lines[0], format!("{:<40} 2 commit(s)", "a"));
        assert_eq!(plan.lines[0].len(), 40 + " 2 commit(s)".len());
        assert!(plan.lines.contains(&"Held back (outside “Verified only”):".to_string()));
        assert!(plan.lines.contains(&"  b — not listed".to_string()));
        assert!(plan.lines.contains(&"Not updated (needs you):".to_string()));
        assert!(plan.lines.contains(&"  c — Has local edits".to_string()));
        let all = plan_plugin_updates(&states, &trust, "all");
        assert_eq!(all.ready.len(), 2);
        assert!(all.held.is_empty());
        // `dict(...)[scope]` raises for a scope SCOPES does not list.
        assert_eq!(plan_plugin_updates(&states, &trust, "nonsense").scope_label, None);
    }

    #[test]
    fn update_plan_trusts_the_listed_repository_not_the_folder_name() {
        let listing = |key: &str, commit: &str| plugins_mod::Listing {
            key: key.into(),
            repo: "https://github.com/owner/x".into(),
            validated_commit: commit.into(),
        };
        let sha = "1111111111111111111111111111111111111111";
        let newer = "2222222222222222222222222222222222222222";
        let st = |id: &str, url: &str, remote: &str| {
            plugins_mod::State::new(id, "update", "behind", 1, url).with_remote(remote)
        };
        let states = vec![
            // the listed repository, at the reviewed commit, in ssh form
            st("same", "git@github.com:Owner/X.git", sha),
            // a folder named like the listing, cloned from somewhere else
            st("impostor", "https://github.com/mallory/x", sha),
            // the listed repository, moved past what was reviewed
            st("moved", "https://github.com/owner/x", newer),
        ];
        let trust: BTreeMap<String, plugins_mod::Listing> = [
            ("same".to_string(), listing("verified", sha)),
            ("impostor".to_string(), listing("verified", sha)),
            ("moved".to_string(), listing("reviewed", sha)),
        ]
        .into();
        let plan = plan_plugin_updates(&states, &trust, "verified");
        assert_eq!(plan.ready.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["same"]);
        assert_eq!(plan.pins.get("same").map(String::as_str), Some(sha));
        assert_eq!(plan.held.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["impostor", "moved"]);
        assert_eq!(plan.held_why[0], "not the listed repository");
        assert!(plan.held_why[1].starts_with("changed since it was reviewed"), "{}", plan.held_why[1]);
        assert!(plan.lines.contains(&"  impostor — not the listed repository".to_string()));
        // "Verified, and changed since review" takes the newer commits and
        // pins nothing; the impostor is still not the listing.
        let changed = plan_plugin_updates(&states, &trust, "changed");
        assert_eq!(changed.ready.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["same", "moved"]);
        assert!(changed.pins.is_empty());
        // "All plugins" asks nothing of provenance.
        assert_eq!(plan_plugin_updates(&states, &trust, "all").ready.len(), 3);
    }
}
