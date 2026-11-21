//! Which programs run in one session, read from /proc.

use std::path::Path;

/// The session id: field 6 of `/proc/<pid>/stat`, counted after the last
/// ')' because the command name before it may hold spaces and parentheses.
fn session_of(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // state ppid pgrp session ...
    rest.split_whitespace().nth(3)?.parse().ok()
}

/// The names every process in session `sid` goes by: its `comm`, the script
/// it runs when it is an interpreter, and the file names in the first words
/// of its command line (a script's `comm` is cut to 15 characters, its
/// command line is not).
pub fn session_commands(proc_root: &Path, sid: u32) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let Ok(dir) = std::fs::read_dir(proc_root) else {
        return names;
    };
    for entry in dir.flatten() {
        let fname = entry.file_name();
        if !fname.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let p = entry.path();
        let Some(s) = std::fs::read_to_string(p.join("stat"))
            .ok()
            .and_then(|st| session_of(&st))
        else {
            continue;
        };
        if s != sid {
            continue;
        }
        let comm = std::fs::read_to_string(p.join("comm")).unwrap_or_default();
        let comm = comm.trim();
        let args: Vec<String> = std::fs::read(p.join("cmdline"))
            .map(|b| {
                b.split(|c| *c == 0)
                    .filter(|a| !a.is_empty())
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut found = if comm.is_empty() {
            Vec::new()
        } else {
            crate::postupdate::program_names(comm, &args)
        };
        found.extend(
            args.iter()
                .filter(|a| !a.starts_with('-'))
                .take(2)
                .map(|a| a.rsplit('/').next().unwrap_or(a).to_string()),
        );
        for n in found {
            if !n.is_empty() && !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(dir: &Path, pid: &str, sid: u32, comm: &str, args: &[&str]) {
        let p = dir.join(pid);
        std::fs::create_dir(&p).unwrap();
        std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(
            p.join("cmdline"),
            args.iter().map(|a| format!("{a}\0")).collect::<String>(),
        )
        .unwrap();
        // pid (comm) state ppid pgrp session ...; comm may hold spaces and ')'.
        std::fs::write(
            p.join("stat"),
            format!("{pid} ({comm} x) S 1 {sid} {sid} 0 -1 4194560\n"),
        )
        .unwrap();
    }

    #[test]
    fn session_commands_reads_a_fake_tree() {
        let dir = tempfile::tempdir().unwrap();
        fake(
            dir.path(),
            "100",
            100,
            "omarchy",
            &["omarchy", "update", "-y"],
        );
        fake(
            dir.path(),
            "101",
            100,
            "omarchy-update-",
            &[
                "/usr/bin/bash",
                "/opt/omarchy/bin/omarchy-update-system-pkgs",
            ],
        );
        fake(dir.path(), "102", 100, "sudo", &["sudo", "pacman", "-Syu"]);
        fake(
            dir.path(),
            "200",
            200,
            "omarchy-snapshot",
            &["omarchy-snapshot"],
        );
        std::fs::create_dir(dir.path().join("self")).unwrap();
        let names = session_commands(dir.path(), 100);
        assert!(
            names.contains(&"omarchy-update-system-pkgs".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"sudo".to_string()));
        assert!(
            !names.contains(&"omarchy-snapshot".to_string()),
            "other session"
        );
        assert!(session_commands(dir.path(), 999).is_empty());
    }
}
