//! A command on a hidden pseudo-terminal, with no GTK in sight.
//!
//! The child gets the terminal as stdin, stdout and stderr and as its
//! controlling terminal, in a session of its own, so programs that insist
//! on a tty (sudo, pacman's progress bars) behave as they do in a terminal
//! window. The caller owns the master side: it writes keystrokes, reads
//! output without blocking, and polls for the exit.

use std::io;
use std::path::Path;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rustix::fs::{fcntl_getfl, fcntl_setfl, open, Mode, OFlags};
use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
use rustix::termios::{tcsetwinsize, Winsize};

use super::procs::{self, Proc};

/// Columns and rows the child sees.
const COLS: u16 = 120;
const ROWS: u16 = 40;

/// How long `Drop` waits for the child to leave after the hangup.
const REAP_GRACE: Duration = Duration::from_secs(2);

/// A running child and the master side of its terminal.
pub struct Pty {
    /// The child's process id; also its session id.
    pub pid: u32,
    master: Option<OwnedFd>,
    child: Child,
    status: Option<i32>,
    /// The child's descendants that were on a terminal when last seen
    /// (`track`): the update's own work, which a hangup is meant to end.
    /// What left the terminal on purpose (`setsid`, a restarted app) is
    /// not here and is left alone.
    known: Vec<Proc>,
}

fn code_of(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|n| 128 + n))
        .unwrap_or(1)
}

impl Pty {
    /// Start `argv` on a new terminal with `env` added to the environment.
    pub fn spawn(argv: &[String], env: &[(String, String)]) -> Result<Pty, String> {
        Pty::spawn_with(argv, env, &[], &[])
    }

    /// Like `spawn`, but the child first loses the inherited variables named
    /// in `env_remove`, then gets `env`. `pretend_inherited` is set before the
    /// removal, to stand in for the parent's environment in tests.
    pub fn spawn_with(
        argv: &[String],
        env: &[(String, String)],
        env_remove: &[&str],
        pretend_inherited: &[(String, String)],
    ) -> Result<Pty, String> {
        let (program, args) = argv.split_first().ok_or("no command to run")?;
        let err = |what: &str, e: rustix::io::Errno| format!("{what}: {e}");

        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)
            .map_err(|e| err("openpt", e))?;
        grantpt(&master).map_err(|e| err("grantpt", e))?;
        unlockpt(&master).map_err(|e| err("unlockpt", e))?;
        let name = ptsname(&master, Vec::new()).map_err(|e| err("ptsname", e))?;
        let slave = open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| err("open the terminal", e))?;
        let size = Winsize {
            ws_row: ROWS,
            ws_col: COLS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        tcsetwinsize(&slave, size).map_err(|e| err("set the terminal size", e))?;

        // Before the spawn, so a failure here cannot leave a child behind.
        let flags = fcntl_getfl(&master).map_err(|e| err("fcntl", e))?;
        fcntl_setfl(&master, flags | OFlags::NONBLOCK).map_err(|e| err("fcntl", e))?;

        let dup = |fd: &OwnedFd| fd.try_clone().map_err(|e| format!("dup: {e}"));
        let mut cmd = Command::new(program);
        cmd.args(args).envs(pretend_inherited.iter().map(|(k, v)| (k, v)));
        for k in env_remove {
            cmd.env_remove(k);
        }
        cmd.envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::from(dup(&slave)?))
            .stdout(Stdio::from(dup(&slave)?))
            .stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls (sigaction, setsid, ioctl)
        // run between fork and exec. Descriptor 0 is the slave by then.
        unsafe {
            cmd.pre_exec(|| {
                // Ctrl-C (Stop, Cancel, Quit) must reach the update even when
                // Almanac inherited it ignored, as a background job does.
                use nix::sys::signal::{signal, SigHandler, Signal};
                for s in [Signal::SIGINT, Signal::SIGQUIT] {
                    signal(s, SigHandler::SigDfl).map_err(io::Error::from)?;
                }
                rustix::process::setsid().map_err(io::Error::from)?;
                let stdin = BorrowedFd::borrow_raw(0);
                rustix::process::ioctl_tiocsctty(stdin).map_err(io::Error::from)?;
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
        Ok(Pty {
            pid: child.id(),
            master: Some(master),
            child,
            status: None,
            known: Vec::new(),
        })
    }

    /// Remember the processes of `tree` (the child's, from
    /// `procs::tree`) that have a controlling terminal, and forget those
    /// that are gone. Drop ends whatever of these is still alive, even
    /// after it left the child's tree.
    pub fn track(&mut self, tree: &[Proc]) {
        let root = Path::new("/proc");
        self.known
            .retain(|k| procs::stat_of(root, k.pid).is_some_and(|p| p.start == k.start));
        for p in tree {
            if p.tty_nr != 0 && !self.known.iter().any(|k| k.pid == p.pid && k.start == p.start) {
                self.known.push(p.clone());
            }
        }
    }

    /// The master side, for a caller that wants to poll it.
    pub fn master_fd(&self) -> BorrowedFd<'_> {
        self.master.as_ref().expect("master lives until drop").as_fd()
    }

    /// Whether this terminal echoes what is typed: a cheap guard only.
    /// Under `omarchy update` the program at this end is `script`, which
    /// puts this (outer) terminal in raw mode, so it reads "off" whatever
    /// happens further in; the guard that matters is sudo's own: it turns
    /// echo off on the terminal it reads the password from (script's inner
    /// one) before it asks.
    pub fn echo_on(&self) -> Result<bool, String> {
        let t = rustix::termios::tcgetattr(self.master_fd()).map_err(|e| format!("tcgetattr: {e}"))?;
        Ok(t.local_modes.contains(rustix::termios::LocalModes::ECHO))
    }

    /// Send `bytes` to the child as if typed.
    pub fn write_all(&self, bytes: &[u8]) -> Result<(), String> {
        let mut rest = bytes;
        while !rest.is_empty() {
            match rustix::io::write(self.master_fd(), rest) {
                Ok(n) => rest = &rest[n..],
                Err(rustix::io::Errno::INTR) => {}
                Err(rustix::io::Errno::AGAIN) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(format!("write: {e}")),
            }
        }
        Ok(())
    }

    /// Append whatever the child has printed so far to `buf` without
    /// blocking. `Ok(0)` means nothing right now, including after the child
    /// has gone (Linux reports EIO once the last slave closes).
    pub fn read_available(&self, buf: &mut Vec<u8>) -> Result<usize, String> {
        let mut total = 0;
        let mut chunk = [0u8; 4096];
        loop {
            match rustix::io::read(self.master_fd(), &mut chunk) {
                Ok(0) => return Ok(total),
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    total += n;
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(rustix::io::Errno::AGAIN) | Err(rustix::io::Errno::IO) => return Ok(total),
                Err(e) => return Err(format!("read: {e}")),
            }
        }
    }

    /// The exit code if the child has finished (a signal n gives 128+n),
    /// else `None`. Stays the same once known.
    pub fn try_wait(&mut self) -> Option<i32> {
        if self.status.is_none() {
            self.status = self.child.try_wait().ok().flatten().map(code_of);
        }
        self.status
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // What runs now, before the hangup scatters it.
        if self.status.is_none() {
            let tree = procs::tree(Path::new("/proc"), self.pid);
            self.track(&tree);
        }
        // Closing the master hangs the terminal up on the child's session.
        self.master = None;
        if self.status.is_none() {
            let deadline = Instant::now() + REAP_GRACE;
            let mut gone = false;
            while Instant::now() < deadline {
                if matches!(self.child.try_wait(), Ok(Some(_)) | Err(_)) {
                    gone = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if !gone {
                // The hangup did not end it.
                self.kill_group();
                self.kill_known();
                let _ = self.child.kill();
                let _ = self.child.wait();
                return;
            }
        }
        // The leader is gone, but a descendant that ignores SIGHUP may not
        // be, and could outlive Almanac holding the update lock. The leader
        // is its own process group, so one SIGKILL takes them all; the group
        // id stays reserved while any member lives, and ESRCH means none do.
        self.kill_group();
        // `omarchy update`'s work is not in that group: `script` runs it on
        // a terminal and in a session of its own (and sudo may add another).
        self.kill_known();
    }
}

impl Pty {
    /// SIGKILL what `track` saw that is still the same process (its start
    /// time matches, so a reused pid is spared), and anything those started
    /// since. Root's processes (pacman under sudo) refuse the signal; they
    /// are not ours to end, and are not what holds Omarchy's update lock.
    fn kill_known(&self) {
        let root = Path::new("/proc");
        let same = |k: &Proc| procs::stat_of(root, k.pid).is_some_and(|p| p.start == k.start);
        let mut targets: Vec<Proc> = self.known.iter().filter(|k| same(k)).cloned().collect();
        let mut i = 0;
        while i < targets.len() {
            for p in procs::tree(root, targets[i].pid).into_iter().skip(1) {
                if !targets.iter().any(|t| t.pid == p.pid) {
                    targets.push(p);
                }
            }
            i += 1;
        }
        for t in &targets {
            if !same(t) {
                continue;
            }
            if let Some(pid) = rustix::process::Pid::from_raw(t.pid as i32) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
    }

    /// SIGKILL every process in the child's group.
    fn kill_group(&self) {
        if let Some(pgid) = rustix::process::Pid::from_raw(self.pid as i32) {
            let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    /// Collect output until the child exits (or ten seconds pass).
    fn run_out(pty: &mut Pty) -> (String, Option<i32>) {
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let _ = pty.read_available(&mut out);
            if let Some(code) = pty.try_wait() {
                let _ = pty.read_available(&mut out);
                return (String::from_utf8_lossy(&out).into_owned(), Some(code));
            }
            if Instant::now() > deadline {
                return (String::from_utf8_lossy(&out).into_owned(), None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_child_runs_on_a_terminal() {
        let mut pty = Pty::spawn(
            &argv(&[
                "sh",
                "-c",
                "test -t 0 && test -t 1 && echo tty-ok; read x; echo got:$x; exit 3",
            ]),
            &[],
        )
        .unwrap();
        pty.write_all(b"hi\n").unwrap();
        let (out, code) = run_out(&mut pty);
        assert!(out.contains("tty-ok"), "{out:?}");
        assert!(out.contains("got:hi"), "{out:?}");
        assert_eq!(code, Some(3));
    }

    /// Almanac started from a shell's background job ignores Ctrl-C, and
    /// that is inherited: Stop, Cancel and Quit sent ^C to an update that
    /// never heard it. The child starts with the default handling.
    #[test]
    fn ctrl_c_reaches_the_child_even_when_almanac_ignores_it() {
        use nix::sys::signal::{signal, SigHandler, Signal};
        // SAFETY: the old handler is put back below.
        let old = unsafe { signal(Signal::SIGINT, SigHandler::SigIgn) }.unwrap();
        let pty = Pty::spawn(&argv(&["sh", "-c", "grep SigIgn /proc/$$/status"]), &[]);
        unsafe { signal(Signal::SIGINT, old) }.unwrap();
        let (out, _) = run_out(&mut pty.unwrap());
        let mask = out
            .split_whitespace()
            .nth(1)
            .and_then(|m| u64::from_str_radix(m, 16).ok())
            .expect("SigIgn read");
        assert_eq!(mask & (1 << (2 - 1)), 0, "SIGINT not ignored: {out:?}");
        assert_eq!(mask & (1 << (3 - 1)), 0, "SIGQUIT not ignored: {out:?}");
    }

    #[test]
    fn the_child_leads_its_own_session() {
        let mut pty = Pty::spawn(&argv(&["sh", "-c", "ps -o sid= -p $$"]), &[]).unwrap();
        let pid = pty.pid;
        let (out, _) = run_out(&mut pty);
        assert_eq!(out.trim().parse::<u32>().ok(), Some(pid), "{out:?}");
    }

    #[test]
    fn the_environment_reaches_the_child() {
        let mut pty = Pty::spawn(
            &argv(&["sh", "-c", "echo v=$ALMANAC_T"]),
            &[("ALMANAC_T".into(), "yes".into())],
        )
        .unwrap();
        let (out, _) = run_out(&mut pty);
        assert!(out.contains("v=yes"), "{out:?}");
    }

    #[test]
    fn dropping_the_master_ends_the_child() {
        let pty = Pty::spawn(&argv(&["sh", "-c", "sleep 30"]), &[]).unwrap();
        let pid = rustix::process::Pid::from_raw(pty.pid as i32).unwrap();
        let started = Instant::now();
        drop(pty);
        // Well under REAP_GRACE: the hangup ended it, not the SIGKILL fallback.
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }

    #[test]
    fn a_descendant_that_ignores_the_hangup_is_killed_too() {
        let pty = Pty::spawn(
            &argv(&["sh", "-c", "trap '' HUP; sleep 30 & echo kid:$!; wait"]),
            &[],
        )
        .unwrap();
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        let kid = loop {
            let _ = pty.read_available(&mut out);
            let text = String::from_utf8_lossy(&out).into_owned();
            if let Some(n) = text.split("kid:").nth(1).and_then(|r| r.split_whitespace().next()) {
                if let Ok(n) = n.parse::<i32>() {
                    break n;
                }
            }
            assert!(Instant::now() < deadline, "{text:?}");
            std::thread::sleep(Duration::from_millis(10));
        };
        let kid = rustix::process::Pid::from_raw(kid).unwrap();
        drop(pty);
        let deadline = Instant::now() + Duration::from_secs(2);
        while rustix::process::test_kill_process(kid).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            rustix::process::test_kill_process(kid),
            Err(rustix::io::Errno::SRCH)
        );
    }

    #[test]
    fn a_missing_program_is_an_error() {
        assert!(Pty::spawn(&argv(&["/no/such/program"]), &[]).is_err());
        assert!(Pty::spawn(&[], &[]).is_err());
    }

    #[test]
    fn a_descendant_that_ignores_the_hangup_is_killed_when_the_leader_dies_of_it() {
        let pty = Pty::spawn(
            &argv(&["sh", "-c", "(trap '' HUP; exec sleep 30) & echo kid:$!; wait"]),
            &[],
        )
        .unwrap();
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        let kid = loop {
            let _ = pty.read_available(&mut out);
            let text = String::from_utf8_lossy(&out).into_owned();
            if let Some(n) = text.split("kid:").nth(1).and_then(|r| r.split_whitespace().next()) {
                if let Ok(n) = n.parse::<i32>() {
                    break n;
                }
            }
            assert!(Instant::now() < deadline, "{text:?}");
            std::thread::sleep(Duration::from_millis(10));
        };
        let kid = rustix::process::Pid::from_raw(kid).unwrap();
        let started = Instant::now();
        drop(pty);
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        let deadline = Instant::now() + Duration::from_secs(2);
        while rustix::process::test_kill_process(kid).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            rustix::process::test_kill_process(kid),
            Err(rustix::io::Errno::SRCH)
        );
    }
}
