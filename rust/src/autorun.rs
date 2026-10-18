//! Port of `autorun.py`.
//!
//! The non-pacman half of the scheduled update.
//!
//! omarchy-auto-update handles repo and AUR packages, with holds for anything
//! that needs a human. This handles the three kinds of software it cannot see:
//! agent CLIs, tracked AppImages and Omarchy shell plugins.
//!
//! It is a separate program rather than more shell because each of these already
//! has working logic in this app -- agents.detect() knows what is behind,
//! appimage.check() knows what a release contains -- and reimplementing that in
//! bash would give two answers that drift.
//!
//! Run with no arguments it does only what auto-update.json enables.

/// `print(msg, flush=True)`.
pub fn log(msg: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

/// Agent CLIs with a self-updater; returns how many were updated.
pub fn update_agents() -> usize {
    todo!("port: autorun::update_agents")
}

/// Tracked AppImages; returns how many were updated.
pub fn update_appimages() -> usize {
    todo!("port: autorun::update_appimages")
}

/// Omarchy shell plugins. Off by default: these change the desktop.
///
/// One at a time, and only those plugins.py classifies as a clean update
/// inside the chosen scope: `omarchy plugin update` with no id would also
/// fast-forward a symlinked development plugin's workspace.
pub fn update_plugins() -> usize {
    todo!("port: autorun::update_plugins")
}

/// The CLI (`--agents`, `--appimages`, `--plugins`). `argv` None reads the
/// process arguments; the arguments exclude the program name. Returns the
/// exit code (always 0, as in the Python).
pub fn main(argv: Option<&[String]>) -> i32 {
    let _ = argv;
    todo!("port: autorun::main")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
