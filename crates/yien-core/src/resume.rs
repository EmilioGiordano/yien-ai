//! Telling a resume that failed from a program that simply ended
//! (RF-SES-11), from the exit code, the time since the launch and the screen
//! the program left. Pure: the caller brings the clock and the screen.

use std::time::Duration;

/// How soon after a resume launch an exit with an error code counts as the
/// resume failing: an agent that resumed and then ended with an error later
/// is the user's session ending, not the resume.
pub(crate) const EXIT_GRACE: Duration = Duration::from_secs(10);

/// How long after the launch the CLI's own "session not found" message
/// still counts. Longer than [`EXIT_GRACE`], since a slow start can print it
/// late; bounded, since a long conversation may quote the same words.
pub(crate) const MESSAGE_GRACE: Duration = Duration::from_secs(60);

/// What an agent CLI prints when the session it was asked to resume is gone
/// (Claude Code: "No conversation found with session ID: …").
const NOT_FOUND_MESSAGES: &[&str] = &["No conversation found"];

/// Why a resume failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeFailure {
    /// The CLI said it has no such session.
    NotFound,
    /// The CLI ended with an error code right after it started.
    ExitedEarly(u32),
}

impl ResumeFailure {
    /// The one-line notice for the user, naming the CLI by `profile`.
    pub fn notice(self, profile: &str) -> String {
        let why = match self {
            Self::NotFound => "conversation not found".to_owned(),
            Self::ExitedEarly(code) => format!("exited with code {code}"),
        };
        format!("{profile} could not resume ({why}); a shell opened. Relaunch for a new session.")
    }

    /// A short name for the logs; never the screen's text.
    pub fn kind(self) -> &'static str {
        match self {
            Self::NotFound => "not found",
            Self::ExitedEarly(_) => "exited early",
        }
    }
}

/// Whether a launch that resumed a session and then ended means the resume
/// failed. `code` is the exit code (`None` when unknown), `elapsed` the time
/// from the launch to the exit, `screen` the lines the program left.
///
/// An unknown code is never a failure on its own: when unsure, the pane
/// shows the exit as it is.
pub(crate) fn resume_failed(
    code: Option<u32>,
    elapsed: Duration,
    screen: &[&str],
) -> Option<ResumeFailure> {
    // A resumed conversation can quote the message; the CLI's own one ends
    // it with an error code, so a clean exit (`/exit`) never counts.
    let said_not_found = elapsed <= MESSAGE_GRACE
        && code != Some(0)
        && screen
            .iter()
            .any(|line| NOT_FOUND_MESSAGES.iter().any(|m| line.contains(m)));
    if said_not_found {
        return Some(ResumeFailure::NotFound);
    }
    match code {
        Some(code) if code != 0 && elapsed <= EXIT_GRACE => Some(ResumeFailure::ExitedEarly(code)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOT_FOUND: &str = "No conversation found with session ID: 4f1c-9a";

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn rf_ses_11_the_not_found_message_with_an_error_exit_is_a_failure() {
        for code in [Some(1), None] {
            assert_eq!(
                resume_failed(code, secs(2), &["", NOT_FOUND, ""]),
                Some(ResumeFailure::NotFound),
                "{code:?}"
            );
        }
    }

    #[test]
    fn rf_ses_11_a_clean_exit_over_a_quoted_message_is_the_session_ending() {
        assert_eq!(resume_failed(Some(0), secs(2), &[NOT_FOUND]), None);
    }

    #[test]
    fn rf_ses_11_a_slow_start_still_counts_the_message_up_to_its_grace() {
        assert_eq!(
            resume_failed(Some(1), MESSAGE_GRACE, &[NOT_FOUND]),
            Some(ResumeFailure::NotFound)
        );
        let late = MESSAGE_GRACE + Duration::from_millis(1);
        assert_eq!(resume_failed(Some(1), late, &[NOT_FOUND]), None);
    }

    #[test]
    fn rf_ses_11_an_early_error_exit_is_a_failure() {
        assert_eq!(
            resume_failed(Some(1), secs(1), &["something else"]),
            Some(ResumeFailure::ExitedEarly(1))
        );
        assert_eq!(
            resume_failed(Some(3221225786), EXIT_GRACE, &[]),
            Some(ResumeFailure::ExitedEarly(3221225786))
        );
    }

    #[test]
    fn rf_ses_11_a_late_exit_or_a_clean_one_is_the_session_ending() {
        let late = EXIT_GRACE + Duration::from_millis(1);
        assert_eq!(resume_failed(Some(1), late, &[]), None);
        assert_eq!(resume_failed(Some(0), secs(1), &[]), None);
    }

    #[test]
    fn an_unknown_exit_without_the_message_is_left_alone() {
        assert_eq!(resume_failed(None, secs(1), &["error"]), None);
    }

    #[test]
    fn the_notice_names_the_cli_and_the_reason_on_one_line() {
        let notice = ResumeFailure::NotFound.notice("Claude Code");
        assert!(notice.starts_with("Claude Code could not resume"));
        assert!(notice.contains("not found"));
        assert!(notice.contains("Relaunch"));
        assert!(!notice.contains('\n'));
        let early = ResumeFailure::ExitedEarly(2).notice("Claude Code");
        assert!(early.contains("code 2"));
        assert_eq!(ResumeFailure::ExitedEarly(2).kind(), "exited early");
    }
}
