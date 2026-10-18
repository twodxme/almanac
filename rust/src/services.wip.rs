//! Background services of the user session: systemd user units.
//!
//! Only units that can actually be switched -- `enabled` or `disabled` in
//! `systemctl --user list-unit-files`, services and timers. Static, generated,
//! transient and alias units have no switch and are left out.
//!
//! Some of these are the session's plumbing. Switching off PipeWire takes all
//! sound with it; switching off the portal breaks file pickers and screen
//! sharing. Those are marked essential, and the page asks first and says what
//! stops working. Omarchy's own units are marked as such, since a later
//! `omarchy update` may expect them.
//!
//! Port of `services.py`.

use serde::{Deserialize, Serialize};

// Exact unit names -> what stops working without it. Names, not prefixes:
// hyprsunset is a blue-light filter and xdg-desktop-portal-rewrite-launchers
// a helper, and a prefix match called both essential.
const SOUND: &str = "All sound, and screen sharing";
const PORTAL: &str = "File pickers, screen sharing and links from apps";
pub const ESSENTIAL: &[(&str, &str)] = &[
    ("pipewire.service", SOUND),
    ("pipewire.socket", SOUND),
    ("pipewire-pulse.service", SOUND),
    ("pipewire-pulse.socket", SOUND),
    ("wireplumber.service", "All sound"),
    ("xdg-desktop-portal.service", PORTAL),
    ("xdg-desktop-portal-hyprland.service", PORTAL),
    ("xdg-desktop-portal-gtk.service", PORTAL),
    ("dbus.service", "Almost everything in the session"),
    ("dbus-broker.service", "Almost everything in the session"),
    ("gnome-keyring-daemon.service", "Saved passwords and SSH keys"),
    ("at-spi-dbus-bus.service", "Screen readers and accessibility tools"),
    ("gvfs-daemon.service", "Network locations and devices in the file manager"),
    ("hyprpolkitagent.service", "Password prompts for system changes"),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Service {
    pub unit: String,
    pub description: String,
    pub enabled: bool,
    pub active: bool,
    /// what stops working, '' if nothing much
    pub essential: String,
    pub omarchy: bool,
}

pub fn available() -> bool {
    crate::util::which("systemctl").is_some()
}

/// `systemctl --user <args>`'s stdout, or "" when it could not run.
#[allow(dead_code)]
fn run(args: &[&str], timeout: u64) -> String {
    let _ = (args, timeout);
    todo!("port: services::_run")
}

pub fn essential_for(unit: &str) -> String {
    ESSENTIAL.iter().find(|(u, _)| *u == unit).map(|(_, s)| (*s).to_owned()).unwrap_or_default()
}

pub fn services() -> Vec<Service> {
    todo!("port: services::services")
}

/// Enable and start, or disable and stop: the switch means both.
pub fn toggle_argv(unit: &str, on: bool) -> Vec<String> {
    vec!["systemctl".into(), "--user".into(),
         if on { "enable" } else { "disable" }.into(), "--now".into(), unit.into()]
}

pub fn is_enabled(unit: &str) -> bool {
    let _ = unit;
    todo!("port: services::is_enabled")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
