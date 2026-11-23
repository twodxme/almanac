//! Which programs the update runs, read from /proc.
//!
//! The update is followed by descent from the process Almanac spawned, not
//! by session: `omarchy update` re-execs itself under `script -qefc`, which
//! runs the real work on a terminal (and in a session) of its own, and sudo
//! (`use_pty`) may start yet another. Only the spawned process's
//! descendants are read, so a scan is a handful of small files, not all of
//! /proc.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

/// One process of the update, from `/proc/<pid>/stat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// non-zero: the process has a controlling terminal
    pub tty_nr: i64,
    /// clock ticks after boot: with `pid`, names the process for good
    /// (a pid alone may be reused once it is gone)
    pub start: u64,
}

/// The fields of a stat line, counted after the last ')' because the
/// command name before it may hold spaces and parentheses.
fn parse_stat(pid: u32, stat: &str) -> Option<Proc> {
    let rest: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    // state ppid pgrp session tty_nr tpgid flags minflt cminflt majflt
    // cmajflt utime stime cutime cstime priority nice num_threads
    // itrealvalue starttime
    if rest.first() == Some(&"Z") || rest.first() == Some(&"X") {
        return None;
    }
    Some(Proc {
        pid,
        ppid: rest.get(1)?.parse().ok()?,
        tty_nr: rest.get(4)?.parse().ok()?,
        start: rest.get(19)?.parse().ok()?,
    })
}

/// `pid` as it is now; None once it is gone (or a zombie).
pub fn stat_of(proc_root: &Path, pid: u32) -> Option<Proc> {
    let stat = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    parse_stat(pid, &stat)
}

/// The children of `pid`, from each of its threads' `children` file. None
/// when the kernel offers no such file (CONFIG_PROC_CHILDREN off).
fn children_of(proc_root: &Path, pid: u32) -> Option<Vec<u32>> {
    let tasks = std::fs::read_dir(proc_root.join(pid.to_string()).join("task")).ok()?;
    let mut out = Vec::new();
    let mut any = false;
    for t in tasks.flatten() {
        let Ok(text) = std::fs::read_to_string(t.path().join("children")) else {
            continue;
        };
        any = true;
        out.extend(
            text.split_whitespace()
                .filter_map(|n| n.parse::<u32>().ok()),
        );
    }
    any.then_some(out)
}

/// `root` and everything below it that is alive, parents before children.
pub fn tree(proc_root: &Path, root: u32) -> Vec<Proc> {
    let Some(first) = stat_of(proc_root, root) else {
        return Vec::new();
    };
    if children_of(proc_root, root).is_none() {
        return tree_by_ppid(proc_root, first);
    }
    let mut out = vec![first];
    let mut queue = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        for kid in children_of(proc_root, pid).unwrap_or_default() {
            if out.iter().any(|p| p.pid == kid) {
                continue;
            }
            if let Some(p) = stat_of(proc_root, kid) {
                out.push(p);
                queue.push_back(kid);
            }
        }
    }
    out
}

/// The same from one pass over every process's parent, for a kernel
/// without `children` files.
fn tree_by_ppid(proc_root: &Path, first: Proc) -> Vec<Proc> {
    let mut by_parent: BTreeMap<u32, Vec<Proc>> = BTreeMap::new();
    if let Ok(dir) = std::fs::read_dir(proc_root) {
        for e in dir.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if let Some(p) = stat_of(proc_root, pid) {
                by_parent.entry(p.ppid).or_default().push(p);
            }
        }
    }
    let mut out = vec![first];
    let mut i = 0;
    while i < out.len() {
        let pid = out[i].pid;
        for kid in by_parent.remove(&pid).unwrap_or_default() {
            out.push(kid);
        }
        i += 1;
    }
    out
}

/// The names `pid` goes by: its `comm`, the script it runs when it is an
/// interpreter, and the file names in the first words of its command line
/// (a script's `comm` is cut to 15 characters, its command line is not).
fn names_of(proc_root: &Path, pid: u32) -> Vec<String> {
    let p = proc_root.join(pid.to_string());
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
    found
}

/// The names every process in `procs` goes by, each once.
pub fn commands_of(proc_root: &Path, procs: &[Proc]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for p in procs {
        for n in names_of(proc_root, p.pid) {
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

    /// A fake /proc entry. `kids` None leaves out the `task/*/children`
    /// files, as on a kernel without them.
    fn fake(dir: &Path, pid: u32, ppid: u32, comm: &str, args: &[&str], kids: Option<&[u32]>) {
        let p = dir.join(pid.to_string());
        std::fs::create_dir(&p).unwrap();
        std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(
            p.join("cmdline"),
            args.iter().map(|a| format!("{a}\0")).collect::<String>(),
        )
        .unwrap();
        // pid (comm) state ppid pgrp session tty_nr ... starttime (field 22)
        std::fs::write(
            p.join("stat"),
            format!(
                "{pid} ({comm} x) S {ppid} {pid} {pid} 34816 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 {} 0\n",
                1000 + pid
            ),
        )
        .unwrap();
        if let Some(kids) = kids {
            let t = p.join("task").join(pid.to_string());
            std::fs::create_dir_all(&t).unwrap();
            let list: Vec<String> = kids.iter().map(u32::to_string).collect();
            std::fs::write(t.join("children"), list.join(" ")).unwrap();
        }
    }

    /// Omarchy's layout: omarchy → script → (new session) bash running the
    /// system-packages script → sudo → pacman. A process outside the tree
    /// is not part of it.
    fn layout(dir: &Path, with_children: bool) {
        let k = |v: &'static [u32]| with_children.then_some(v);
        fake(
            dir,
            100,
            1,
            "script",
            &["script", "-qefc", "omarchy update -y"],
            k(&[101]),
        );
        fake(
            dir,
            101,
            100,
            "bash",
            &["bash", "/usr/bin/omarchy-update", "-y"],
            k(&[102]),
        );
        fake(
            dir,
            102,
            101,
            "omarchy-update-",
            &[
                "/usr/bin/bash",
                "/opt/omarchy/bin/omarchy-update-system-pkgs",
            ],
            k(&[103]),
        );
        fake(dir, 103, 102, "sudo", &["sudo", "pacman", "-Syu"], k(&[]));
        fake(
            dir,
            200,
            1,
            "omarchy-snapshot",
            &["omarchy-snapshot"],
            k(&[]),
        );
        std::fs::create_dir(dir.join("self")).unwrap();
    }

    #[test]
    fn the_tree_follows_children_across_sessions() {
        let dir = tempfile::tempdir().unwrap();
        layout(dir.path(), true);
        let t = tree(dir.path(), 100);
        let pids: Vec<u32> = t.iter().map(|p| p.pid).collect();
        assert_eq!(pids, [100, 101, 102, 103]);
        assert_eq!(t[3].start, 1103);
        assert_eq!(t[1].ppid, 100);
        assert_ne!(t[1].tty_nr, 0);
        let names = commands_of(dir.path(), &t);
        assert!(
            names.contains(&"omarchy-update-system-pkgs".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"sudo".to_string()));
        assert!(!names.contains(&"omarchy-snapshot".to_string()), "not ours");
        assert!(tree(dir.path(), 999).is_empty());
    }

    #[test]
    fn without_children_files_the_parents_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        layout(dir.path(), false);
        let pids: Vec<u32> = tree(dir.path(), 100).iter().map(|p| p.pid).collect();
        assert_eq!(pids, [100, 101, 102, 103]);
    }

    #[test]
    fn a_zombie_is_gone() {
        assert!(parse_stat(5, "5 (x) Z 1 5 5 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 0").is_none());
        let p = parse_stat(
            5,
            "5 (a) b) c) S 1 5 5 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 0",
        )
        .unwrap();
        assert_eq!((p.ppid, p.start), (1, 77));
    }

    #[test]
    fn this_process_is_found_in_the_real_proc() {
        let me = std::process::id();
        let t = tree(Path::new("/proc"), me);
        assert_eq!(t.first().map(|p| p.pid), Some(me));
        assert_eq!(
            stat_of(Path::new("/proc"), me).map(|p| p.start),
            t.first().map(|p| p.start)
        );
    }
}
