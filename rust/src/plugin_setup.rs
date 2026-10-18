//! Port of `plugin_setup.py`.
//!
//! What a plugin still needs after it is installed, read from its README.
//!
//! The marketplace marks some plugins "requires additional setup" and stops
//! there. Installing them is the same `omarchy plugin add` as any other; the
//! setup is whatever their README asks for next. This reads the README from the
//! installed checkout -- local, nothing fetched -- and sorts what it finds:
//!
//! * **packages**  `pacman -S` / `yay -S` / `paru -S` lines: installed by
//!   Almanac itself, snapshotted and verified like any other install.
//! * **commands**  build steps, setup scripts and `systemctl --user` lines from
//!   the README's code blocks, run in a terminal in the plugin's directory --
//!   after the user has seen every line, and the scripts' own contents.
//! * **by hand**   what cannot be done for the user: API keys and logins,
//!   config and keybinding edits. Listed, with the README heading to read.
//!
//! Only code blocks are read for commands: prose is not an instruction. A line
//! is taken only when it starts with a known tool, so an example output block
//! or a snippet of QML is never run. `sudo` lines are kept as written -- the
//! terminal shows the password prompt, and nothing is elevated silently.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[allow(dead_code)]
static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?ms)^```[^\n]*\n(.*?)^```").unwrap());
#[allow(dead_code)]
static PKG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:sudo\s+)?(?:pacman|yay|paru)\s+-S\w*\s+(.+)$").unwrap());
#[allow(dead_code)]
static PKG_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9@][a-z0-9@._+-]*$").unwrap());
/// Commands worth running for the user, by the tool they start with.
#[allow(dead_code)]
static RUNNABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:sudo\s+)?(?:make|cmake|meson|ninja|cargo|go|npm|pnpm|yarn|bun|uv|pip|",
        r"pipx|python3?|gcc|cc|systemctl\s+--user|bash\s+\S+\.sh|sh\s+\S+\.sh|",
        r"\./[\w./-]+)\b"
    ))
    .unwrap()
});
/// Never run, whatever the README says.
#[allow(dead_code)]
static NEVER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(curl|wget)[^|]*\|\s*(sudo\s+)?(ba|z)?sh|rm\s+-rf\s+[/~]|",
        r"omarchy[- ]plugin|git\s+clone|cd\s"
    ))
    .unwrap()
});
/// What only a person can do: (pattern, label).
#[allow(dead_code)]
static BY_HAND: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (Regex::new(concat!(
            r"(?i)\b(api[ _-]?key|access token|secret|password|log ?in|sign ?in|",
            r"credential|oauth)\b"
        )).unwrap(), "Credentials or a sign-in"),
        (Regex::new(concat!(
            r"(?i)\b(keybind|bindings?\.(conf|lua)|hyprland\.(conf|lua)|",
            r"config\.(toml|json|ya?ml))\b"
        )).unwrap(), "Configuration you edit"),
    ]
});
#[allow(dead_code)]
static HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(#{1,6})\s+(.+?)\s*#*$").unwrap());
/// Only these parts of a README are instructions for a user installing it.
/// Everything else -- development, tests, uninstalling -- is not setup, and a
/// README's `cargo clippy` or `uninstall.sh` must never be offered as such.
#[allow(dead_code)]
static SETUP_SECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)install|setup|set up|getting started|quick ?start|",
        r"requirement|dependenc|prerequisite|build|configur|usage"
    ))
    .unwrap()
});
#[allow(dead_code)]
static OTHER_SECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)develop|contribut|test|uninstall|remov|troubleshoot|",
        r"faq|changelog|release|licen|hack|debug|ci\b|lint"
    ))
    .unwrap()
});
/// Commands that are development chores even inside an install section.
#[allow(dead_code)]
static CHORE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(test|fmt|clippy|lint|check|bench|uninstall)\b").unwrap());

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SetupPlan {
    pub plugin_dir: PathBuf,
    /// file name, '' when there is none
    pub readme: String,
    pub packages: Vec<String>,
    pub commands: Vec<String>,
    /// path -> contents, shown first. Insertion-ordered like the Python dict;
    /// serialised as a JSON object.
    #[serde(with = "ordered_map")]
    pub scripts: Vec<(String, String)>,
    /// (what, README heading)
    pub by_hand: Vec<(String, String)>,
}

impl SetupPlan {
    pub fn automatable(&self) -> bool {
        !self.packages.is_empty() || !self.commands.is_empty()
    }

    pub fn empty(&self) -> bool {
        !(self.automatable() || !self.by_hand.is_empty())
    }
}

/// `Vec<(String, String)>` as a JSON object, keeping order on the way out.
mod ordered_map {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[(String, String)], s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(v.iter().map(|(k, x)| (k, x)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(String, String)>, D::Error> {
        let m = std::collections::BTreeMap::<String, String>::deserialize(d)?;
        Ok(m.into_iter().collect())
    }
}

#[allow(dead_code)]
fn _readme(_plugin_dir: &Path) -> Option<PathBuf> {
    todo!("port: plugin_setup::_readme")
}

/// Code-block lines, with backslash continuations joined and prompts cut.
#[allow(dead_code)]
fn _logical_lines(_block: &str) -> Vec<String> {
    todo!("port: plugin_setup::_logical_lines")
}

/// (start, heading path) for every heading, outermost first.
#[allow(dead_code)]
fn _sections(_text: &str) -> Vec<(usize, Vec<String>)> {
    todo!("port: plugin_setup::_sections")
}

/// A section is setup when its heading path says so and nothing in it
/// says it is about something else.
#[allow(dead_code)]
fn _is_setup(_path: &[String]) -> bool {
    todo!("port: plugin_setup::_is_setup")
}

/// The setup plan for the plugin installed in `plugin_dir`, from its README.
/// Err when the README (or a script it names) cannot be read -- the Python
/// lets that OSError propagate.
pub fn plan(_plugin_dir: &Path) -> Result<SetupPlan, String> {
    todo!("port: plugin_setup::plan")
}

/// The commands, as one shell script that stops at the first failure.
pub fn terminal_script(_p: &SetupPlan) -> String {
    todo!("port: plugin_setup::terminal_script")
}

/// Everything the plan would do, for the confirmation dialog.
pub fn describe(_p: &SetupPlan) -> String {
    todo!("port: plugin_setup::describe")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
