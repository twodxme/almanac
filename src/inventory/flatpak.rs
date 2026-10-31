//! Installed Flatpak apps.

use super::*;

/// By path: PATH puts user-writable folders ahead of /usr/bin.
pub(super) const FLATPAK_BIN: &str = "/usr/bin/flatpak";

/// Installed Flatpak apps. Silent no-op when flatpak is not installed, so
/// the section simply does not appear rather than erroring.
/// (Err: flatpak's output is not UTF-8, `_run`'s UnicodeDecodeError.)
pub fn flatpaks() -> Result<Vec<App>, String> {
    let out = run(
        &[
            FLATPAK_BIN,
            "list",
            "--app",
            "--columns=application,name,version",
        ],
        30,
    )?;
    if py_strip(&out).is_empty() {
        return Ok(Vec::new());
    }
    let mut pending = HashSet::new();
    for line in py_splitlines(&run(
        &[
            FLATPAK_BIN,
            "remote-ls",
            "--updates",
            "--columns=application",
        ],
        60,
    )?) {
        if !py_strip(line).is_empty() {
            pending.insert(py_strip(line).to_string());
        }
    }

    let mut apps = Vec::new();
    for line in py_splitlines(&out) {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.is_empty() || py_strip(parts[0]).is_empty() {
            continue;
        }
        let appid = py_strip(parts[0]).to_string();
        let name = if parts.len() > 1 {
            py_strip(parts[1]).to_string()
        } else {
            appid.clone()
        };
        let version = if parts.len() > 2 {
            py_strip(parts[2]).to_string()
        } else {
            "?".into()
        };
        apps.push(App {
            name: if name.is_empty() { appid.clone() } else { name },
            version: if version.is_empty() {
                "?".into()
            } else {
                version
            },
            source: FLATPAK.into(),
            description: appid.clone(),
            update: if pending.contains(&appid) {
                "available".into()
            } else {
                String::new()
            },
            exec_path: appid.clone(),
            extra: json!({"icon": appid, "is_app": true, "appid": appid})
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into(),
        });
    }
    Ok(apps)
}
