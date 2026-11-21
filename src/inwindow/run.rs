//! Drives `omarchy update` on a hidden terminal without any GTK: a view pumps
//! [`UpdateRun::poll`] on a timer and reacts to the [`Event`]s.
//!
//! Passwords: sudo reads the password from the terminal it runs on, which is
//! our PTY. The driver only notices the request and writes what the caller
//! hands over; the secret is never kept, logged or echoed (sudo turns echo
//! off, so it does not come back in the output either).
//!
//! Reboots: Almanac never agrees to one. `answer` refuses a reboot "yes", and
//! a gum confirm that appears while `omarchy-update-restart` runs is declined
//! by the driver itself whatever its wording (Omarchy translates its strings).

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::procs::session_commands;
use super::prompt::{self, Answer, Prompt};
use super::pty::Pty;
use super::tracker::{Timeline, Tracker};

/// How long the output must be quiet before the tail is checked for a prompt.
const PROMPT_IDLE: Duration = Duration::from_millis(400);
/// Lines of output kept in memory (the log file keeps everything).
const TAIL_LINES: usize = 2000;
/// Characters of recent output a prompt is looked for in.
const PROMPT_WINDOW: usize = 4096;
/// A single unterminated line is cut to this many bytes in memory.
const PARTIAL_MAX: usize = 64 * 1024;
const RESTART_PROCESS: &str = "omarchy-update-restart";
const RESTART_NOTE: &str =
    "A restart is needed. Almanac did not restart your computer. Run omarchy-system reboot when ready.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Output or the running programs changed; re-read the timeline.
    Changed,
    /// sudo wants a password. `attempt` is 1 for the first request.
    NeedsPassword { prompt: String, attempt: u32 },
    /// The update waits on a question (always a `Prompt::Question`).
    Question(Prompt),
    /// The update ended. The terminal is already released.
    Finished { code: i32 },
}

pub struct UpdateRun {
    pty: Option<Pty>,
    sid: u32,
    tracker: Tracker,
    log: File,
    /// Complete lines, newest last.
    tail: VecDeque<String>,
    /// The line being printed.
    partial: String,
    /// Bytes of an unfinished UTF-8 character at the end of the last read.
    carry: Vec<u8>,
    /// Recent output for prompt detection.
    window: String,
    last_output: Instant,
    commands: Vec<String>,
    /// sudo's "Sorry, try again." lines seen, plus one.
    attempt: u32,
    /// The prompt already raised or answered, and the password attempt it
    /// belongs to. gum redraws its question in place, so the same prompt must
    /// not be raised again until the screen shows something else.
    handled: Option<(Prompt, u32)>,
    /// What was last raised and awaits `answer`/`answer_password`.
    open: Option<Prompt>,
}

fn open_log(path: &Path) -> Result<File, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(f)
}

impl UpdateRun {
    /// Run `argv` on a new terminal with `env` added. A normal run leaves
    /// `OMARCHY_UPDATE_UNATTENDED` alone.
    pub fn start(
        argv: Vec<String>,
        env: Vec<(String, String)>,
        timeline: Timeline,
        log_path: PathBuf,
    ) -> Result<UpdateRun, String> {
        let log = open_log(&log_path)?;
        let pty = Pty::spawn(&argv, &env)?;
        let sid = pty.pid;
        Ok(UpdateRun {
            pty: Some(pty),
            sid,
            tracker: Tracker::new(timeline),
            log,
            tail: VecDeque::new(),
            partial: String::new(),
            carry: Vec::new(),
            window: String::new(),
            last_output: Instant::now(),
            commands: Vec::new(),
            attempt: 1,
            handled: None,
            open: None,
        })
    }

    /// A run that repairs a stuck update: Omarchy may ask its questions
    /// (`OMARCHY_UPDATE_UNATTENDED=0`); reboot questions are still declined.
    pub fn resolve(
        argv: Vec<String>,
        mut env: Vec<(String, String)>,
        timeline: Timeline,
        log_path: PathBuf,
    ) -> Result<UpdateRun, String> {
        env.push(("OMARCHY_UPDATE_UNATTENDED".into(), "0".into()));
        UpdateRun::start(argv, env, timeline, log_path)
    }

    pub fn timeline(&self) -> &Timeline {
        self.tracker.timeline()
    }

    pub fn sid(&self) -> u32 {
        self.sid
    }

    pub fn is_running(&self) -> bool {
        self.pty.is_some()
    }

    /// The last 2 000 lines of output, as printed (control sequences kept).
    pub fn log_tail(&self) -> String {
        let mut out = String::new();
        for l in &self.tail {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&self.partial);
        out
    }

    pub fn poll(&mut self) -> Vec<Event> {
        self.poll_at(Instant::now())
    }

    pub(crate) fn poll_at(&mut self, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(pty) = self.pty.as_ref() else {
            return events;
        };
        let mut buf = Vec::new();
        let mut changed = pty.read_available(&mut buf).unwrap_or(0) > 0;
        if changed {
            self.last_output = now;
            self.take_output(&buf);
        }

        let commands = session_commands(Path::new("/proc"), self.sid);
        if commands != self.commands {
            self.tracker.on_procs(&commands);
            self.commands = commands;
            changed = true;
        }

        if let Some(code) = self.pty.as_mut().and_then(Pty::try_wait) {
            // Whatever it printed last.
            let mut rest = Vec::new();
            if let Some(p) = self.pty.as_ref() {
                let _ = p.read_available(&mut rest);
            }
            self.take_output(&rest);
            self.tracker.on_exit(code);
            // Release the terminal now: its Drop signals the process group,
            // and a reaped leader's group id could be reused later.
            self.pty = None;
            self.open = None;
            events.push(Event::Changed);
            events.push(Event::Finished { code });
            return events;
        }
        if changed {
            events.push(Event::Changed);
        }

        if now.saturating_duration_since(self.last_output) >= PROMPT_IDLE {
            if let Some(ev) = self.check_prompt() {
                events.push(ev);
            }
        }
        events
    }

    /// Feed output to the log, the tail, the prompt window and the tracker.
    fn take_output(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let _ = self.log.write_all(bytes);
        self.carry.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.carry) {
            Ok(_) => self.carry.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            // A byte that is not UTF-8 at all: keep going, lossily.
            Err(_) => self.carry.len(),
        };
        let rest = self.carry.split_off(valid);
        let text = String::from_utf8_lossy(&self.carry).into_owned();
        self.carry = rest;
        if text.is_empty() {
            return;
        }

        self.tracker.on_output(&text);

        self.window.push_str(&text);
        if self.window.len() > PROMPT_WINDOW {
            let mut cut = self.window.len() - PROMPT_WINDOW;
            while !self.window.is_char_boundary(cut) {
                cut += 1;
            }
            self.window.drain(..cut);
        }

        self.partial.push_str(&text);
        while let Some(at) = self.partial.find('\n') {
            let mut line: String = self.partial.drain(..=at).collect();
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
            if line.contains("Sorry, try again.") {
                self.attempt += 1;
            }
            if self.tail.len() == TAIL_LINES {
                self.tail.pop_front();
            }
            self.tail.push_back(line);
        }
        if self.partial.len() > PARTIAL_MAX {
            let mut cut = self.partial.len() - PARTIAL_MAX;
            while !self.partial.is_char_boundary(cut) {
                cut += 1;
            }
            self.partial.drain(..cut);
        }
    }

    fn check_prompt(&mut self) -> Option<Event> {
        let found = prompt::detect(&self.window);
        let Some(p) = found else {
            self.handled = None;
            return None;
        };
        let key = (p.clone(), self.attempt);
        if self.handled.as_ref() == Some(&key) {
            return None;
        }
        match p {
            Prompt::Password { prompt } => {
                self.handled = Some(key);
                self.open = Some(Prompt::Password { prompt: prompt.clone() });
                Some(Event::NeedsPassword { prompt, attempt: self.attempt })
            }
            Prompt::Question { ref answers, .. } => {
                let restarting = self.commands.iter().any(|c| c == RESTART_PROCESS);
                let gum = answers.iter().all(|a| !a.send.ends_with(b"\n"));
                let reboot = answers.iter().any(|a| a.is_reboot_yes) || (restarting && gum);
                if reboot {
                    self.handled = Some(key);
                    let no = answers
                        .iter()
                        .find(|a| !a.is_reboot_yes && (a.send == b"n" || a.send == b"n\n"))
                        .cloned();
                    // No way to say No: say nothing. Never guess.
                    if let Some(no) = no {
                        if self.write(&no.send).is_ok() {
                            self.tracker.set_restart_note(RESTART_NOTE.into());
                            return Some(Event::Changed);
                        }
                    }
                    return None;
                }
                self.handled = Some(key);
                self.open = Some(p.clone());
                Some(Event::Question(p))
            }
        }
    }

    fn write(&self, bytes: &[u8]) -> Result<(), String> {
        self.pty
            .as_ref()
            .ok_or_else(|| "the update is not running".to_string())?
            .write_all(bytes)
    }

    /// Type the password (`Some`) or cancel the request with Ctrl-C (`None`).
    /// The secret lives only for this call.
    pub fn answer_password(&mut self, secret: Option<&str>) -> Result<(), String> {
        self.open = None;
        match secret {
            Some(s) => {
                let mut line = Vec::with_capacity(s.len() + 1);
                line.extend_from_slice(s.as_bytes());
                line.push(b'\n');
                let r = self.write(&line);
                line.fill(0);
                r
            }
            None => self.write(b"\x03"),
        }
    }

    /// Send one of a question's answers. An answer that agrees to a reboot is
    /// refused and nothing is sent.
    pub fn answer(&mut self, a: &Answer) -> Result<(), String> {
        if a.is_reboot_yes {
            return Err("Almanac never agrees to a restart for you.".into());
        }
        self.open = None;
        self.write(&a.send)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use crate::inwindow::tracker::{AppState, StepState};
    use std::time::Duration;

    fn timeline() -> Timeline {
        Timeline::new(
            &[
                ("cursor".to_string(), "1.3".to_string(), false),
                ("mise".to_string(), "2.0".to_string(), true),
            ],
            false,
        )
    }

    /// Write `body` as a bash script into `dir` and return the argv for it.
    fn script(dir: &Path, body: &str) -> Vec<String> {
        let p = dir.join("fake-update.sh");
        std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
        vec!["bash".to_string(), p.to_string_lossy().into_owned()]
    }

    /// Pump `run` until it finishes (or a generous deadline), letting
    /// `react` answer what it raises. Returns every event seen.
    fn drive(run: &mut UpdateRun, mut react: impl FnMut(&Event, &mut UpdateRun)) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(40);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            for e in run.poll() {
                react(&e, run);
                let done = matches!(e, Event::Finished { .. });
                seen.push(e);
                if done {
                    return seen;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the fake update did not finish in time; events: {seen:?}\n{}", run.log_tail());
    }

    fn finished_code(events: &[Event]) -> i32 {
        events
            .iter()
            .find_map(|e| match e {
                Event::Finished { code } => Some(*code),
                _ => None,
            })
            .expect("Finished")
    }

    const STEPS: &str = r#"
(exec -a omarchy-snapshot sleep 0.3)
(exec -a omarchy-update-keyring sleep 0.3)
(exec -a omarchy-update-system-pkgs sleep 0.3) &
echo ':: Retrieving packages...'
echo '(1/2) upgrading cursor'
wait
(exec -a omarchy-migrate sleep 0.3)
(exec -a omarchy-update-aur-pkgs sleep 0.3) &
echo '(1/1) upgrading mise'
wait
(exec -a omarchy-update-boot sleep 0.3)
"#;

    #[test]
    fn a_clean_fake_update_completes() {
        let dir = tempfile::tempdir().unwrap();
        let argv = script(dir.path(), STEPS);
        let mut run =
            UpdateRun::start(argv, vec![], timeline(), dir.path().join("logs/run.log")).unwrap();
        let events = drive(&mut run, |_, _| {});
        assert_eq!(finished_code(&events), 0);
        let t = run.timeline();
        for (id, s) in &t.steps {
            assert!(matches!(s, StepState::Done { .. }), "{id:?} is {s:?}");
        }
        for r in &t.apps {
            assert!(matches!(r.state, AppState::Done { .. }), "{} is {:?}", r.name, r.state);
        }
        assert!(events.iter().any(|e| matches!(e, Event::Changed)));
    }

    #[test]
    fn a_failing_update_reports_its_code() {
        let dir = tempfile::tempdir().unwrap();
        let argv = script(dir.path(), "echo oops\nexit 3");
        let mut run = UpdateRun::start(argv, vec![], timeline(), dir.path().join("r.log")).unwrap();
        let events = drive(&mut run, |_, _| {});
        assert_eq!(finished_code(&events), 3);
        assert!(run.timeline().steps.iter().any(|(_, s)| matches!(s, StepState::Failed { .. })));
    }

    #[test]
    fn a_question_is_raised_and_answered() {
        let dir = tempfile::tempdir().unwrap();
        let argv = script(
            dir.path(),
            "echo downloading\nprintf ':: Proceed with installation? [Y/n] '\nread -r a\necho \"answer=$a\"",
        );
        let mut run = UpdateRun::start(argv, vec![], timeline(), dir.path().join("r.log")).unwrap();
        let mut raised = 0;
        let events = drive(&mut run, |e, run| {
            if let Event::Question(Prompt::Question { text, answers }) = e {
                raised += 1;
                assert_eq!(text, ":: Proceed with installation?");
                run.answer(&answers[0]).unwrap();
            }
        });
        assert_eq!(finished_code(&events), 0);
        assert_eq!(raised, 1, "the same question is raised once");
        assert!(run.log_tail().contains("answer=y"), "{}", run.log_tail());
    }

    #[test]
    fn a_password_is_asked_on_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("r.log");
        let argv = script(
            dir.path(),
            "printf '[sudo] password for tim: '; stty -echo; read pw; stty echo; [ \"$pw\" = secret ] && echo ok",
        );
        let mut run = UpdateRun::start(argv, vec![], timeline(), log.clone()).unwrap();
        let mut asked = Vec::new();
        let events = drive(&mut run, |e, run| {
            if let Event::NeedsPassword { prompt, attempt } = e {
                asked.push((prompt.clone(), *attempt));
                run.answer_password(Some("secret")).unwrap();
            }
        });
        assert_eq!(finished_code(&events), 0, "{}", run.log_tail());
        assert_eq!(asked, vec![("[sudo] password for tim:".to_string(), 1)]);
        assert!(!run.log_tail().contains("secret"));
        assert!(!std::fs::read_to_string(&log).unwrap().contains("secret"));
    }

    #[test]
    fn a_wrong_password_raises_attempt_two() {
        let dir = tempfile::tempdir().unwrap();
        let argv = script(
            dir.path(),
            "stty -echo\nprintf '[sudo] password for tim: '\nread pw\n\
             printf '\\nSorry, try again.\\n[sudo] password for tim: '\nread pw\nstty echo\n[ \"$pw\" = right ] && echo ok",
        );
        let mut run = UpdateRun::start(argv, vec![], timeline(), dir.path().join("r.log")).unwrap();
        let mut attempts = Vec::new();
        let events = drive(&mut run, |e, run| {
            if let Event::NeedsPassword { attempt, .. } = e {
                attempts.push(*attempt);
                let pw = if *attempt == 1 { "wrong" } else { "right" };
                run.answer_password(Some(pw)).unwrap();
            }
        });
        assert_eq!(finished_code(&events), 0, "{}", run.log_tail());
        assert_eq!(attempts, vec![1, 2]);
    }

    #[test]
    fn cancelling_a_password_sends_ctrl_c() {
        let dir = tempfile::tempdir().unwrap();
        let argv = script(
            dir.path(),
            "trap 'echo got-int; exit 130' INT\nprintf '[sudo] password for tim: '\nread pw\necho not-cancelled",
        );
        let mut run = UpdateRun::start(argv, vec![], timeline(), dir.path().join("r.log")).unwrap();
        let events = drive(&mut run, |e, run| {
            if matches!(e, Event::NeedsPassword { .. }) {
                run.answer_password(None).unwrap();
            }
        });
        assert_eq!(finished_code(&events), 130, "{}", run.log_tail());
        assert!(run.log_tail().contains("got-int"));
    }

    /// A gum confirm in a language the wording check does not know, with
    /// `omarchy-update-restart` running as it does in the real update.
    const GUM_REBOOT: &str = r#"
(exec -a omarchy-update-restart sleep 30) &
sleep_pid=$!
printf ' Red\303\251marrer maintenant ?\r\n\n  Yes    No  \r\n\n\342\206\220 toggle \342\200\242 enter submit\r'
stty -icanon -echo
read -r -n1 ans
kill $sleep_pid 2>/dev/null
if [ "$ans" = n ]; then echo declined; else echo "REBOOTING:$ans"; fi
"#;

    #[test]
    fn reboot_yes_is_never_sent() {
        let dir = tempfile::tempdir().unwrap();
        let argv = script(dir.path(), GUM_REBOOT);
        let mut run =
            UpdateRun::resolve(argv, vec![], timeline(), dir.path().join("r.log")).unwrap();
        let events = drive(&mut run, |_, _| {});
        assert_eq!(finished_code(&events), 0, "{}", run.log_tail());
        assert!(run.log_tail().contains("declined"), "{}", run.log_tail());
        assert!(!run.log_tail().contains("REBOOTING"));
        assert!(run.timeline().restart_note.is_some());
        assert!(
            !events.iter().any(|e| matches!(e, Event::Question(_))),
            "a reboot question is not for the user"
        );
    }

    #[test]
    fn a_reboot_question_is_declined_in_a_normal_run_too() {
        let dir = tempfile::tempdir().unwrap();
        let argv = script(dir.path(), GUM_REBOOT);
        let mut run = UpdateRun::start(argv, vec![], timeline(), dir.path().join("r.log")).unwrap();
        let events = drive(&mut run, |_, _| {});
        assert_eq!(finished_code(&events), 0);
        assert!(run.log_tail().contains("declined"));
        assert!(run.timeline().restart_note.is_some());
        assert!(!events.iter().any(|e| matches!(e, Event::Question(_))));
    }

    #[test]
    fn answer_refuses_reboot_yes_and_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let argv = script(
            dir.path(),
            "stty -echo\nif read -r -t 2 x; then echo \"GOT:$x\"; else echo NOTHING; fi",
        );
        let mut run = UpdateRun::start(argv, vec![], timeline(), dir.path().join("r.log")).unwrap();
        let yes = Answer { label: "Yes".into(), send: b"y\n".to_vec(), is_reboot_yes: true };
        assert!(run.answer(&yes).is_err());
        let events = drive(&mut run, |_, _| {});
        assert_eq!(finished_code(&events), 0);
        assert!(run.log_tail().contains("NOTHING"), "{}", run.log_tail());
    }

    #[test]
    fn environment_is_minimal_and_resolve_is_unattended_off() {
        let dir = tempfile::tempdir().unwrap();
        let body = "echo \"unattended=${OMARCHY_UPDATE_UNATTENDED-unset}\"\n\
                    echo \"direct=${OMARCHY_ALLOW_DIRECT_PACMAN-unset}\"\n\
                    echo \"askpass=${SUDO_ASKPASS-unset}\"\necho \"extra=$EXTRA\"";
        let env = vec![("EXTRA".to_string(), "yes".to_string())];

        let mut run = UpdateRun::start(
            script(dir.path(), body),
            env.clone(),
            timeline(),
            dir.path().join("a.log"),
        )
        .unwrap();
        drive(&mut run, |_, _| {});
        let t = run.log_tail();
        assert!(t.contains("unattended=unset"), "{t}");
        assert!(t.contains("direct=unset"), "{t}");
        assert!(t.contains("askpass=unset"), "{t}");
        assert!(t.contains("extra=yes"), "{t}");

        let mut run =
            UpdateRun::resolve(script(dir.path(), body), env, timeline(), dir.path().join("b.log"))
                .unwrap();
        drive(&mut run, |_, _| {});
        let t = run.log_tail();
        assert!(t.contains("unattended=0"), "{t}");
        assert!(t.contains("direct=unset"), "{t}");
    }

    #[test]
    fn log_tail_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("deep/er/run.log");
        let argv = script(dir.path(), "for i in $(seq 1 10000); do echo \"line $i\"; done");
        let mut run = UpdateRun::start(argv, vec![], timeline(), log.clone()).unwrap();
        drive(&mut run, |_, _| {});
        let tail = run.log_tail();
        assert!(tail.lines().count() <= 2000, "{} lines", tail.lines().count());
        assert!(tail.contains("line 10000"));
        assert!(!tail.contains("line 1\n"));
        let full = std::fs::read_to_string(&log).unwrap();
        assert!(full.contains("line 1\r\n") && full.contains("line 10000"));
        assert_eq!(full.lines().count(), 10000);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn the_terminal_is_released_once_finished() {
        let dir = tempfile::tempdir().unwrap();
        let argv = script(dir.path(), "echo hi");
        let mut run = UpdateRun::start(argv, vec![], timeline(), dir.path().join("r.log")).unwrap();
        drive(&mut run, |_, _| {});
        assert!(!run.is_running());
        assert!(run.poll().is_empty());
        assert!(run.answer(&Answer { label: "No".into(), send: b"n\n".to_vec(), is_reboot_yes: false }).is_err());
    }
}
