//! Recognises a question the update's output is waiting on, so the window can
//! show it as buttons. No GTK here.
//!
//! Only the last non-empty segment of the output (split on `\r` and `\n`) is
//! looked at: a prompt is, by definition, the last thing printed.

use std::sync::LazyLock;

use regex::Regex;

use crate::postupdate::clean_output;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// A password request. The answer is typed by the user, never guessed.
    Password { prompt: String },
    /// A question with a fixed set of answers.
    Question { text: String, answers: Vec<Answer> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub label: String,
    /// Exact bytes to write to the PTY.
    pub send: Vec<u8>,
    /// This answer agrees to a reboot.
    pub is_reboot_yes: bool,
}

static PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)password[^:]*:\s*$").unwrap());
static YES_NO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\[(?:Y/n|y/N|y/n)\]\??\s*$").unwrap());
static SELECTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:enter a (?:selection|number)|\(default=[^)]*\))[^:]*:\s*$").unwrap());
static GUM_CHOICES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Yes\s+No$").unwrap());

fn answer(label: &str, send: &[u8], is_reboot_yes: bool) -> Answer {
    Answer { label: label.to_string(), send: send.to_vec(), is_reboot_yes }
}

fn mentions_reboot(text: &str) -> bool {
    text.to_lowercase().contains("reboot")
}

pub fn detect(tail: &str) -> Option<Prompt> {
    let cleaned = clean_output(tail);
    let mut segments: Vec<&str> = cleaned
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    // gum confirm ends with a key-help line under the choices; it is part of
    // the prompt, not output that came after it.
    if segments.last().is_some_and(|s| s.contains("toggle") && s.contains("submit")) {
        segments.pop();
    }
    let last = *segments.last()?;

    if GUM_CHOICES.is_match(last) {
        let text = segments.len().checked_sub(2).map(|i| segments[i]).unwrap_or("");
        let reboot = mentions_reboot(text);
        return Some(Prompt::Question {
            text: text.to_string(),
            answers: vec![answer("Yes", b"y", reboot), answer("No", b"n", false)],
        });
    }
    if PASSWORD.is_match(last) {
        return Some(Prompt::Password { prompt: last.to_string() });
    }
    if YES_NO.is_match(last) {
        let text = YES_NO.replace(last, "").trim().to_string();
        let reboot = mentions_reboot(&text);
        return Some(Prompt::Question {
            text,
            answers: vec![answer("Yes", b"y\n", reboot), answer("No", b"n\n", false)],
        });
    }
    if SELECTION.is_match(last) {
        return Some(Prompt::Question {
            text: last.to_string(),
            answers: vec![answer("Default", b"\n", false)],
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(p: Option<Prompt>) -> (String, Vec<Answer>) {
        match p {
            Some(Prompt::Question { text, answers }) => (text, answers),
            other => panic!("expected a question, got {other:?}"),
        }
    }

    /// `gum confirm "Reboot now?"` as recorded on a PTY (styling kept).
    fn gum(question: &str) -> String {
        format!(
            "\x1b[?25l\r\x1b[J \x1b[38;2;127;187;179;48;2;45;53;59;1m{question}\x1b[m\r\n\n \
             \x1b[48;2;61;72;77m  \x1b[38;2;211;198;170mYes\x1b[39m    \x1b[m \
             \x1b[48;2;45;53;59m  \x1b[38;2;211;198;170mNo\x1b[39m  \x1b[m\r\n\n\
             \x1b[38;2;98;98;98m\u{2190}\x1b[m \x1b[38;2;74;74;74mtoggle\x1b[38;2;60;60;60m \u{2022} \
             \x1b[38;2;98;98;98menter\x1b[m \x1b[38;2;74;74;74msubmit\x1b[38;2;60;60;60m \u{2022} \
             \x1b[38;2;98;98;98my\x1b[m \x1b[38;2;74;74;74mYes\x1b[m\r\x1b[4A"
        )
    }

    #[test]
    fn yes_no_defaults() {
        let (text, answers) = question(detect("downloading...\n:: Proceed with installation? [Y/n] "));
        assert_eq!(text, ":: Proceed with installation?");
        assert_eq!(answers, vec![answer("Yes", b"y\n", false), answer("No", b"n\n", false)]);
    }

    #[test]
    fn replace_conflict() {
        let (text, answers) =
            question(detect(":: python-x and python-y are in conflict. Remove python-y? [y/N] "));
        assert!(text.ends_with("Remove python-y?"));
        assert_eq!(answers[0].send, b"y\n");
        assert_eq!(answers[1].send, b"n\n");
    }

    #[test]
    fn selection() {
        let (_, answers) = question(detect("1) a\n2) b\nEnter a selection (default=all): "));
        assert_eq!(answers, vec![answer("Default", b"\n", false)]);
    }

    #[test]
    fn sudo_password() {
        assert_eq!(
            detect("[sudo] password for tim: "),
            Some(Prompt::Password { prompt: "[sudo] password for tim:".to_string() })
        );
    }

    #[test]
    fn gum_confirm() {
        let (text, answers) = question(detect(&gum("Continue with the update?")));
        assert_eq!(text, "Continue with the update?");
        assert_eq!(answers, vec![answer("Yes", b"y", false), answer("No", b"n", false)]);
    }

    #[test]
    fn gum_confirm_without_help_line() {
        let (text, answers) = question(detect("Install it?\n  Yes    No  "));
        assert_eq!(text, "Install it?");
        assert_eq!(answers[0].send, b"y");
    }

    #[test]
    fn reboot_yes_is_marked() {
        let (_, answers) = question(detect(&gum("Reboot now to finish the update?")));
        assert!(answers[0].is_reboot_yes);
        assert!(!answers[1].is_reboot_yes);
        let (_, answers) = question(detect("Reboot to apply? [Y/n] "));
        assert!(answers[0].is_reboot_yes);
    }

    #[test]
    fn plain_output_is_not_a_prompt() {
        assert_eq!(detect("installing foo\nresolving dependencies...\n"), None);
        assert_eq!(detect(""), None);
        assert_eq!(detect("\x1b[32mdone\x1b[m\n\n"), None);
    }

    #[test]
    fn an_answered_question_is_not_a_prompt() {
        assert_eq!(detect(":: Proceed? [Y/n] y\n:: Retrieving packages...\n"), None);
    }
}
