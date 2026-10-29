//! The scheduled update script (packaging/omarchy-auto-update), run for real
//! in a sandbox: every command it could change the system with -- yay,
//! omarchy, sudo, notifications, almanac-autorun -- is a stub on PATH that
//! writes down how it was called, and its config, state, power supplies and
//! pacman log are files in a temporary directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/omarchy-auto-update");

const NEWS: &str = "Mkinitcpio >=42 needs manual intervention";
const OLDER: &str = "Older news";

/// A feed as archlinux.org serves it, newest first, each item `(title as
/// the feed has it, days old)`.
fn feed(items: &[(&str, i64)]) -> String {
    let now = chrono::Utc::now();
    let mut xml = String::from(
        "<?xml version=\"1.0\"?><rss><channel><title>Arch Linux: Recent news updates</title>",
    );
    for (title, days) in items {
        let date = (now - chrono::Duration::days(*days)).to_rfc2822();
        xml.push_str(&format!("<item><title>{title}</title><pubDate>{date}</pubDate></item>"));
    }
    xml + "</channel></rss>"
}

/// The usual feed: the newest title escaped twice, then an older post.
fn usual_feed() -> String {
    feed(&[("Mkinitcpio &amp;gt;=42 needs manual intervention", 2), (OLDER, 20)])
}

fn have(tool: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {tool}")]).output().is_ok_and(|o| o.status.success())
}

/// bash and python3 are what the script runs on; a system without them
/// cannot run it, so there is nothing to test.
fn runnable() -> bool {
    let ok = have("bash") && have("python3") && have("flock");
    if !ok {
        eprintln!("skipped: bash, python3 or flock missing");
    }
    ok
}

struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    /// A machine on mains power, unmetered, with passwordless sudo, one
    /// routine update pending and no news the user has not read.
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("almanac-auto-update-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["bin", "home", "state", "config", "power/AC"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let sb = Sandbox { dir };
        sb.write("power/AC/type", "Mains\n");
        sb.write("power/AC/online", "1\n");
        sb.write("repo", "foo 1.0-1 -> 1.1-1\n");
        sb.write("aur", "");
        sb.write("feed", &usual_feed());
        sb.write("state/news-seen", &format!("{NEWS}\n{OLDER}\n"));
        let calls = sb.path("calls");
        let log = |what: &str| format!("#!/bin/sh\nprintf '%s\\n' \"{what} $*\" >>'{}'\n", calls.display());
        sb.stub("checkupdates", &format!(
            "{}[ -s '{r}' ] || exit 2\ncat '{r}'\n", log("checkupdates"), r = sb.path("repo").display()));
        sb.stub("yay", &format!(
            "{}[ -s '{a}' ] || exit 1\ncat '{a}'\n", log("yay"), a = sb.path("aur").display()));
        sb.stub("curl", &format!("#!/bin/sh\ncat '{}'\n", sb.path("feed").display()));
        sb.stub("omarchy", &log("omarchy"));
        sb.stub("omarchy-notification-send", &log("notify"));
        sb.stub("logger", &log("logger"));
        sb.stub("sudo", "#!/bin/sh\nexit 0\n");
        sb.stub("busctl", "#!/bin/sh\necho 'u 4'\n");
        sb.stub("nmcli", "#!/bin/sh\nexit 1\n");
        // Core packages are the kernel here; --record-hold is written down.
        sb.stub("almanac-autorun", &format!(
            "#!/bin/sh\ncase \"$1\" in\n\
             --core-holds) grep -E '^linux ' | cut -d' ' -f1 ;;\n\
             --record-hold) printf 'record %s | %s\\n' \"$2\" \"$3\" >>'{c}' ;;\n\
             *) echo autorun-run >>'{c}' ;;\nesac\n", c = calls.display()));
        sb
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        std::fs::write(self.path(rel), text).unwrap();
    }

    fn stub(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = self.path("bin").join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        let d = |rel: &str| self.path(rel);
        Command::new("bash")
            .arg(SCRIPT)
            .args(args)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", d("bin").display()))
            .env("HOME", d("home"))
            .env("LANG", "C.UTF-8")
            .env("XDG_CONFIG_HOME", d("config"))
            .env("XDG_STATE_HOME", d("home/.local/state"))
            .env("OMARCHY_AUTO_UPDATE_CONFIG", d("config/auto-update.json"))
            .env("OMARCHY_AUTO_UPDATE_STATE", d("state"))
            .env("OMARCHY_AUTO_UPDATE_AUTORUN", d("bin/almanac-autorun"))
            .env("OMARCHY_AUTO_UPDATE_POWER_SUPPLY", d("power"))
            .env("OMARCHY_AUTO_UPDATE_PACMAN_LOG", d("pacman.log"))
            .env("OMARCHY_AUTO_UPDATE_OMARCHY", d("bin/omarchy"))
            // Where the user unit starts it without WorkingDirectory=.
            .current_dir(d("home"))
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.path("calls")).unwrap_or_default()
    }

    /// Whether a stub was run: its lines start with its name.
    fn ran(&self, name: &str) -> bool {
        self.calls().lines().any(|l| l == name || l.starts_with(&format!("{name} ")))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.path("state/last-run.log")).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn assert_applied(sb: &Sandbox) {
    assert!(sb.calls().lines().any(|l| l == "omarchy update -y"), "not applied:\n{}\n{}", sb.calls(), sb.log());
}

fn assert_not_applied(sb: &Sandbox) {
    assert!(!sb.ran("omarchy"), "applied:\n{}\n{}", sb.calls(), sb.log());
}

#[test]
fn a_routine_batch_with_the_news_read_is_applied() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("routine");
    let out = sb.run(&[]);
    assert!(out.status.success(), "{out:?}\n{}", sb.log());
    assert_applied(&sb);
    let calls = sb.calls();
    assert!(calls.contains("autorun-run"), "{calls}");
    assert!(calls.contains("notify -u normal Updated 1 packages"), "{calls}");
    assert!(!calls.contains("record "), "a clean run is not a hold:\n{calls}");
}

#[test]
fn unread_arch_news_holds_the_update() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("news-unread");
    sb.write("state/news-seen", &format!("Some older headline\n{OLDER}\n"));
    let out = sb.run(&[]);
    assert!(out.status.success(), "{out:?}");
    assert_not_applied(&sb);
    let calls = sb.calls();
    let held = format!("Not applied automatically. Arch news: {NEWS}. Read it in Almanac (Updates) or run: omarchy update");
    // The notification body is markup to the daemon: the headline's ">" is escaped there.
    let shown = held.replace('>', "&gt;");
    assert!(calls.contains(&format!("notify -u critical 1 updates held {shown}")), "{calls}");
    assert!(calls.contains(&format!("record 1 updates held | {held}")), "{calls}");
    assert!(sb.log().contains(&format!("HELD — Arch news: {NEWS}")), "{}", sb.log());
    // The headline is remembered as the one held for, not as read.
    assert_eq!(std::fs::read_to_string(sb.path("state/news-held")).unwrap(), format!("{NEWS}\n"));
    assert_eq!(std::fs::read_to_string(sb.path("state/news-seen")).unwrap(), format!("Some older headline\n{OLDER}\n"));
}

/// "Read" is per post: the newest one read does not make an older one,
/// a manual intervention perhaps, read too.
#[test]
fn an_older_unread_post_holds_even_when_the_newest_is_read() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("news-older-unread");
    sb.write("state/news-seen", &format!("{NEWS}\n"));
    sb.run(&[]);
    assert_not_applied(&sb);
    assert!(sb.log().contains(&format!("HELD — Arch news: {OLDER}")), "{}", sb.log());
    // Every unread post is named, newest first.
    sb.write("state/news-seen", "");
    sb.run(&[]);
    assert!(sb.log().contains(&format!("HELD — Arch news: {NEWS}; {OLDER}")), "{}", sb.log());
    assert_eq!(std::fs::read_to_string(sb.path("state/news-held")).unwrap(), format!("{NEWS}\n{OLDER}\n"));
}

/// Only the window Almanac lists (news.rs RECENT_DAYS) holds: a hold is
/// always for a post Almanac shows, so it can be cleared there.
#[test]
fn news_older_than_almanac_lists_does_not_hold() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("news-old");
    sb.write("feed", &feed(&[(NEWS, 61), (OLDER, 400)]));
    sb.write("state/news-seen", "");
    sb.run(&[]);
    assert_applied(&sb);
    // ...and a post with no date Almanac can read is not listed there either.
    sb.write("feed", "<rss><channel><item><title>Undated</title><pubDate>soon</pubDate></item></channel></rss>");
    sb.run(&[]);
    assert!(!sb.log().contains("Arch news"), "{}", sb.log());
}

/// Both sides normalise a headline the same way: entities decoded, every
/// run of whitespace (a no-break space from &nbsp; too) one space. A
/// headline Almanac marked read matches the feed's spelling of it.
#[test]
fn a_headline_with_odd_spacing_marked_read_counts_as_read() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("news-nbsp");
    sb.write("feed", &feed(&[("Foo&amp;nbsp;42  needs\n manual intervention", 2)]));
    sb.write("state/news-seen", "Foo 42 needs manual intervention\n");
    sb.run(&[]);
    assert_applied(&sb);
}

/// Lines written before the script decoded entities carry them; the feed
/// escapes twice. Both sides are compared decoded.
#[test]
fn news_marked_read_with_entities_still_counts_as_read() {
    if !runnable() {
        return;
    }
    for seen in ["Mkinitcpio &gt;=42 needs manual intervention", "Mkinitcpio &amp;gt;=42 needs manual intervention"] {
        let sb = Sandbox::new("news-entities");
        sb.write("state/news-seen", &format!("Other\n{seen}\n{OLDER}\n"));
        sb.run(&[]);
        assert_applied(&sb);
        assert!(!sb.calls().contains("Arch news"), "{}", sb.calls());
    }
}

/// A full upgrade run by hand after the hold means someone looked: the
/// next run counts the headline as read rather than holding every week.
#[test]
fn a_full_upgrade_by_hand_after_a_news_hold_clears_it() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("news-by-hand");
    sb.write("state/news-seen", &format!("{OLDER}\n"));
    sb.write("state/news-held", &format!("{NEWS}\n"));
    let st = Command::new("touch").args(["-d", "2026-01-01 00:00"]).arg(sb.path("state/news-held")).status().unwrap();
    assert!(st.success());
    // Before the hold: still held.
    sb.write("pacman.log", "[2025-12-30T10:00:00+0000] [PACMAN] starting full system upgrade\n");
    sb.run(&[]);
    assert_not_applied(&sb);
    // After it, but answered "n" at the prompt: nothing was upgraded, still held.
    sb.write("pacman.log", "[2026-02-01T10:00:00+0000] [PACMAN] starting full system upgrade\n");
    sb.run(&[]);
    assert_not_applied(&sb);
    // After it, and the transaction completed: read.
    sb.write(
        "pacman.log",
        "[2026-02-01T10:00:00+0000] [PACMAN] starting full system upgrade\n\
         [2026-02-01T10:00:30+0000] [ALPM] transaction started\n\
         [2026-02-01T10:01:00+0000] [ALPM] upgraded foo (1.0-1 -> 1.1-1)\n\
         [2026-02-01T10:01:02+0000] [ALPM] transaction completed\n",
    );
    sb.run(&[]);
    assert_applied(&sb);
    assert_eq!(std::fs::read_to_string(sb.path("state/news-seen")).unwrap(), format!("{OLDER}\n{NEWS}\n"));
}

#[test]
fn news_that_cannot_be_read_holds_the_update() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("news-unreadable");
    sb.write("feed", "<html>502 Bad Gateway</html>");
    sb.run(&[]);
    assert_not_applied(&sb);
    assert!(sb.log().contains("HELD — Arch news could not be read"), "{}", sb.log());
}

#[test]
fn a_settings_file_that_cannot_be_read_allows_nothing() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("bad-config");
    sb.write("config/auto-update.json", "{not json");
    sb.run(&[]);
    assert_not_applied(&sb);
    assert!(sb.log().contains("package updates disabled"), "{}", sb.log());
}

#[test]
fn a_core_package_holds_the_batch() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("core");
    sb.write("repo", "foo 1.0-1 -> 1.1-1\nlinux 6.1-1 -> 6.2-1\n");
    sb.run(&[]);
    assert_not_applied(&sb);
    let calls = sb.calls();
    assert!(calls.contains("notify -u critical 2 updates held Not applied automatically. needs a reboot or touches boot: linux. Run: omarchy update"),
            "{calls}");
    assert!(calls.contains("record 2 updates held | "), "{calls}");
}

/// An almanac-autorun that cannot read a line (exit 3), or none at all,
/// holds the whole batch: what it cannot classify might be the kernel.
#[test]
fn core_packages_that_cannot_be_told_hold_everything() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("core-unknown");
    // kmod is core in Almanac's list; a fallback list here once missed it.
    sb.write("repo", "kmod 33-1 -> 34-1\n");
    sb.stub("almanac-autorun", &format!(
        "#!/bin/sh\ncase \"$1\" in\n--core-holds) exit 3 ;;\n\
         --record-hold) printf 'record %s | %s\\n' \"$2\" \"$3\" >>'{c}' ;;\nesac\n",
        c = sb.path("calls").display()));
    sb.run(&[]);
    assert_not_applied(&sb);
    assert!(sb.log().contains("HELD — Almanac could not tell which of them touch boot"), "{}", sb.log());

    let sb = Sandbox::new("core-missing");
    sb.write("repo", "foo 1.0-1 -> 1.1-1\n");
    std::fs::remove_file(sb.path("bin/almanac-autorun")).unwrap();
    sb.run(&[]);
    assert_not_applied(&sb);
    assert!(sb.log().contains("HELD — Almanac could not tell"), "{}", sb.log());
}

/// AUR packages run build scripts anyone can publish: not installed
/// unattended unless the setting says so.
#[test]
fn an_aur_update_is_only_announced_unless_allowed() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("aur");
    sb.write("aur", "some-tool-bin 1.0-1 -> 1.1-1\n");
    sb.run(&[]);
    assert_not_applied(&sb);
    let calls = sb.calls();
    assert!(calls.contains("notify -u normal 2 updates ready Not applied automatically: AUR updates (some-tool-bin)"),
            "{calls}");
    assert!(calls.contains("record 2 updates ready | "), "{calls}");

    sb.write("config/auto-update.json", r#"{"aur": true}"#);
    sb.run(&[]);
    assert_applied(&sb);
}

/// The batch is applied with omarchy update; on plain Arch it is
/// announced, and the way to run it by hand is pacman's.
#[test]
fn without_omarchy_updates_are_only_announced() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("no-omarchy");
    std::fs::remove_file(sb.path("bin/omarchy")).unwrap();
    sb.run(&[]);
    let calls = sb.calls();
    assert!(!calls.lines().any(|l| l.starts_with("omarchy ")), "{calls}");
    assert!(calls.contains("Omarchy is not installed. Run: sudo pacman -Syu"), "{calls}");
    assert!(sb.log().contains("HELD — omarchy not found"), "{}", sb.log());
}

/// python3 runs isolated: a module left in the directory the unit starts
/// in (home) is not imported in place of the standard one.
#[test]
fn python_does_not_import_from_the_working_directory() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("python-isolated");
    let marker = sb.path("imported");
    for module in ["json", "html", "email"] {
        sb.write(&format!("home/{module}.py"), &format!("open('{}', 'a').write('{module}\\n')\n", marker.display()));
    }
    sb.run(&[]);
    assert_applied(&sb);
    assert!(!marker.exists(), "imported from the working directory: {:?}", std::fs::read_to_string(&marker));
}

/// The units: run from /, and a timer left enabled after Almanac is
/// removed skips instead of failing on a script that is gone.
#[test]
fn the_units_start_in_root_and_skip_when_the_script_is_gone() {
    let unit = |name: &str| std::fs::read_to_string(format!("{}/packaging/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let service = unit("omarchy-auto-update.service");
    assert!(service.lines().any(|l| l == "WorkingDirectory=/"), "{service}");
    for text in [service, unit("omarchy-auto-update.timer")] {
        assert!(text.lines().any(|l| l == "ConditionPathExists=/usr/lib/almanac/omarchy-auto-update"), "{text}");
    }
}

#[test]
fn on_battery_nothing_runs_at_all() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("battery");
    sb.write("power/AC/online", "0\n");
    let out = sb.run(&[]);
    assert!(out.status.success());
    let calls = sb.calls();
    assert!(calls.contains("notify -u normal Scheduled update skipped On battery power."), "{calls}");
    assert!(calls.contains("record Scheduled update skipped | On battery power."), "{calls}");
    for not in ["autorun-run", "checkupdates", "yay", "omarchy"] {
        assert!(!sb.ran(not), "{not} ran on battery:\n{calls}");
    }
    assert!(sb.log().contains("SKIPPED — on battery power"));
}

/// A peripheral's own battery (scope Device) says nothing about the
/// machine, and a desktop with no supplies at all is on mains.
#[test]
fn only_the_machines_own_supplies_count_as_power() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("power-device");
    std::fs::remove_dir_all(sb.path("power/AC")).unwrap();
    std::fs::create_dir_all(sb.path("power/mouse")).unwrap();
    sb.write("power/mouse/type", "USB\n");
    sb.write("power/mouse/online", "0\n");
    sb.write("power/mouse/scope", "Device\n");
    sb.run(&[]);
    assert_applied(&sb);
}

#[test]
fn a_metered_connection_skips_the_run() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("metered");
    sb.stub("busctl", "#!/bin/sh\necho 'u 3'\n");
    sb.run(&[]);
    let calls = sb.calls();
    assert!(calls.contains("Scheduled update skipped The connection is metered."), "{calls}");
    assert!(!sb.ran("autorun-run") && !sb.ran("omarchy"), "{calls}");
}

#[test]
fn without_passwordless_sudo_a_routine_batch_is_only_announced() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("no-sudo");
    sb.stub("sudo", "#!/bin/sh\nexit 1\n");
    sb.run(&[]);
    assert_not_applied(&sb);
    let calls = sb.calls();
    assert!(calls.contains("notify -u normal 1 updates ready Routine batch, but automatic install needs passwordless sudo."),
            "{calls}");
    assert!(calls.contains("record 1 updates ready | "), "{calls}");
}

#[test]
fn a_failed_check_holds_and_is_recorded() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("check-failed");
    sb.stub("checkupdates", "#!/bin/sh\nexit 1\n");
    sb.run(&[]);
    assert_not_applied(&sb);
    let calls = sb.calls();
    assert!(calls.contains("record Updates not checked | checkupdates failed (exit 1). Run: omarchy update"), "{calls}");
}

#[test]
fn a_failed_update_is_recorded_and_exits_non_zero() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("update-failed");
    sb.stub("omarchy", &format!("#!/bin/sh\necho \"omarchy $*\" >>'{}'\nexit 1\n", sb.path("calls").display()));
    let out = sb.run(&[]);
    assert!(!out.status.success());
    let calls = sb.calls();
    assert!(calls.contains("notify -u critical Update failed"), "{calls}");
    assert!(calls.contains("record Update failed | "), "{calls}");
}

/// --dry-run prints the plan and changes nothing: no update, no
/// almanac-autorun run, no notification, no History entry, no state.
#[test]
fn a_dry_run_prints_the_plan_and_changes_nothing() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("dry-run");
    let out = sb.run(&["--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("(dry run)"), "{stdout}");
    assert!(stdout.contains("would run: "), "{stdout}");
    assert!(stdout.contains("would apply 1 updates: omarchy update -y\n  foo 1.0-1 -> 1.1-1"), "{stdout}");
    let calls = sb.calls();
    for not in ["omarchy", "notify", "record", "autorun-run"] {
        assert!(!sb.ran(not), "{not} in a dry run:\n{calls}");
    }
    assert!(!sb.path("state/last-run.log").exists());
    assert!(!sb.path("state/lock").exists());

    // A hold is shown as what it would notify and record.
    sb.write("state/news-seen", "");
    let out = sb.run(&["--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("would notify (critical): 1 updates held"), "{stdout}");
    assert!(stdout.contains("would record in History: 1 updates held"), "{stdout}");
    assert!(!sb.path("state/news-held").exists());
}

#[test]
fn an_unknown_argument_is_refused() {
    if !runnable() {
        return;
    }
    let sb = Sandbox::new("bad-arg");
    let out = sb.run(&["--now"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(sb.calls().is_empty());
}

/// The real almanac-autorun: `--record-hold` puts the hold on the History
/// log, marked as a warning, and nothing else.
#[test]
fn record_hold_writes_a_warning_to_the_history_log() {
    let dir = std::env::temp_dir().join(format!("almanac-record-hold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_almanac-autorun"))
            .args(args)
            .env("HOME", &dir)
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .output()
            .unwrap()
    };
    let out = run(&["--record-hold", "3 updates held", "Arch news: something"]);
    assert!(out.status.success(), "{out:?}");
    let log = std::fs::read_to_string(Path::new(&dir).join("state/almanac/history.jsonl")).unwrap();
    let event: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(event["kind"], "autoupdate");
    assert_eq!(event["title"], "3 updates held");
    assert_eq!(event["detail"], "Arch news: something");
    assert_eq!(event["severity"], "warn");

    let out = run(&["--record-hold", "only a title"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(dir.join("state/almanac/history.jsonl")).unwrap(), log);
    let _ = std::fs::remove_dir_all(&dir);
}
