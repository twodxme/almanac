//! Demo mode never touches the machine (ARCHITECTURE.md): the one gate every
//! action the window can take goes through.

use crate::i18n::tr;
use crate::tasks::{self, OnDone, Step};

use super::window::Win;

impl Win {
    /// True, having said so, when this is a demo (`ALMANAC_DEMO`): the
    /// caller then changes nothing. Every action that would install,
    /// remove, switch, run or write something asks this first.
    pub fn demo_refuses(&self) -> bool {
        if !crate::demo::active() {
            return false;
        }
        self.toast(
            &tr("This is a demo: nothing on this machine was changed"),
            5,
        );
        true
    }

    /// `tasks::run` in this window: the task dialog and its commands. In a
    /// demo nothing runs (and `on_done` is not called: nothing was done).
    pub fn run_task(&self, title: &str, steps: Vec<Step>, on_done: Option<OnDone>) {
        if self.demo_refuses() {
            return;
        }
        let _ = tasks::run(&self.window, title, steps, on_done);
    }
}

#[cfg(test)]
mod tests {
    /// (file, its source) for every section that acts on the machine.
    const FILES: &[(&str, &str)] = &[
        ("plugins/removal.rs", include_str!("../plugins/removal.rs")),
        (
            "plugins/controls.rs",
            include_str!("../plugins/controls.rs"),
        ),
        ("plugins/page.rs", include_str!("../plugins/page.rs")),
        ("plugins/updates.rs", include_str!("../plugins/updates.rs")),
        ("actions/link.rs", include_str!("../actions/link.rs")),
        ("actions/update.rs", include_str!("../actions/update.rs")),
        ("actions/rows.rs", include_str!("../actions/rows.rs")),
        (
            "actions/appimage_add.rs",
            include_str!("../actions/appimage_add.rs"),
        ),
        ("store_actions.rs", include_str!("../store_actions.rs")),
        ("history.rs", include_str!("../history.rs")),
        ("ai.rs", include_str!("../ai.rs")),
        ("switchboard.rs", include_str!("../switchboard.rs")),
        ("today.rs", include_str!("../today.rs")),
    ];

    fn code(src: &str) -> &str {
        src.split("#[cfg(test)]").next().unwrap_or(src)
    }

    /// With ALMANAC_DEMO set, Health check's "Fix" ran pkexec for real. Every
    /// task now runs through `run_task`, which refuses in a demo.
    #[test]
    fn every_task_runs_through_the_demo_gate() {
        for (file, src) in FILES {
            assert!(
                !code(src).contains("tasks::run("),
                "{file} runs a task around the demo gate"
            );
            assert!(
                !code(src).contains("tasks::run_each("),
                "{file} runs a task around the demo gate"
            );
        }
        let gate = include_str!("guard.rs");
        let run_task = gate.split("pub fn run_task(").nth(1).unwrap_or("");
        assert!(run_task.find("demo_refuses()") < run_task.find("tasks::run("));
    }

    /// The actions that change the machine without a task dialog ask the
    /// gate first thing.
    #[test]
    fn direct_actions_ask_the_demo_gate_first() {
        let wanted: &[(&str, &[&str])] = &[
            (
                "plugins/removal.rs",
                &[
                    "pub fn on_launch(",
                    "pub fn on_clean_orphans(",
                    "pub fn on_install(",
                    "pub fn on_install_item(",
                    "pub fn install_package(",
                    "pub fn on_remove(",
                ],
            ),
            (
                "plugins/controls.rs",
                &[
                    "pub fn edit_shortcut(",
                    "pub fn on_restart_shell(",
                    "pub fn toggle_plugin(",
                ],
            ),
            ("actions/link.rs", &["pub fn on_install_link("]),
            (
                "actions/update.rs",
                &[
                    "pub fn remove_appimage_then(",
                    "pub fn on_update_one(",
                    "pub fn update_agent(",
                    "pub fn on_git_pull(",
                    "pub fn on_update_appimage(",
                ],
            ),
            (
                "actions/rows.rs",
                &[
                    "pub fn on_self_update(",
                    "pub fn row_pull(",
                    "pub fn row_stash(",
                ],
            ),
            ("actions/appimage_add.rs", &["pub fn on_add_appimage("]),
            (
                "store_actions.rs",
                &[
                    "pub fn run_setup(",
                    "pub fn apply_theme_now(",
                    "pub fn install_store_appimage(",
                    "pub fn remove_store_appimage(",
                ],
            ),
            (
                "history.rs",
                &[
                    "pub fn on_clear_log(",
                    "fn run_fixes(",
                    "pub fn maint_fix(",
                    "pub fn reclaim_space(",
                ],
            ),
            ("ai.rs", &["pub fn ai_act("]),
        ];
        for (file, fns) in wanted {
            let src = FILES
                .iter()
                .find(|(f, _)| f == file)
                .map(|(_, s)| *s)
                .unwrap_or("");
            for f in *fns {
                let body = code(src)
                    .split(f)
                    .nth(1)
                    .unwrap_or_else(|| panic!("{file}: {f} not found"));
                let head: String = body.chars().take(220).collect();
                let first = head
                    .split_once("{\n")
                    .map(|(_, rest)| rest.trim_start())
                    .unwrap_or("");
                assert!(
                    first.starts_with("if self.demo_refuses()"),
                    "{file}: {f} acts before asking the demo gate"
                );
            }
        }
        // The switches snap back instead of switching.
        for file in ["plugins/page.rs", "switchboard.rs"] {
            let src = FILES
                .iter()
                .find(|(f, _)| *f == file)
                .map(|(_, s)| *s)
                .unwrap_or("");
            assert!(
                code(src).contains("w.demo_refuses()"),
                "{file}: a switch acts in a demo"
            );
        }
    }
}
