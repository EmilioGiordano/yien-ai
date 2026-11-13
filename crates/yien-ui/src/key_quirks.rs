//! The one table of per-program key quirks: encodings a pane's program needs
//! that no mode it sets announces, by the program its pane head names.

use yien_input::KeyQuirks;

use crate::model::PaneView;
use crate::pane_head::program_tag;

/// The key quirks of the program `pane`'s head names.
///
/// ```ignore
/// assert!(pane_key_quirks(&claude_pane).ctrl_delete_as_meta_d);
/// ```
pub(crate) fn pane_key_quirks(pane: &PaneView) -> KeyQuirks {
    key_quirks(program_tag(pane))
}

/// The key quirks of `program`, as [`program_tag`] names it; none for a
/// program the table does not know.
fn key_quirks(program: &str) -> KeyQuirks {
    if program.eq_ignore_ascii_case("claude") {
        // Claude Code deletes one character for a Win32 Ctrl+Delete and for
        // CSI 3;5~; only Meta+D deletes the next word.
        KeyQuirks {
            ctrl_delete_as_meta_d: true,
        }
    } else {
        KeyQuirks::default()
    }
}

#[cfg(test)]
mod tests {
    use yien_proto::{PaneId, ProfileId};

    use super::*;
    use crate::model::AgentState;

    fn pane(name: &str, profile: &str, runs_session: bool) -> PaneView {
        PaneView {
            id: PaneId::new(1),
            name: name.into(),
            profile: String::new(),
            cwd: None,
            profile_id: ProfileId::new(profile),
            state: AgentState::Idle,
            needs_reason: None,
            running: true,
            exit_code: None,
            exited: false,
            needs_you_since: None,
            attention_since: None,
            question: None,
            signal_lost: false,
            session_id: None,
            runs_session,
        }
    }

    #[test]
    fn rf_pan_06_claude_code_gets_meta_d_for_ctrl_delete() {
        assert!(key_quirks("claude").ctrl_delete_as_meta_d);
        assert!(key_quirks("Claude").ctrl_delete_as_meta_d);
    }

    #[test]
    fn rf_pan_06_other_programs_keep_the_usual_ctrl_delete() {
        for program in [
            "codex",
            "opencode",
            "powershell",
            "pwsh",
            "cmd",
            "nvim",
            "vim",
            "claude-code",
            "",
        ] {
            assert_eq!(key_quirks(program), KeyQuirks::default(), "{program}");
        }
    }

    #[test]
    fn rf_pan_06_a_pane_gets_the_quirks_of_the_program_running_there() {
        // A Claude Code profile pane, renamed or not, while its session runs.
        assert!(pane_key_quirks(&pane("claude", "claude", true)).ctrl_delete_as_meta_d);
        assert!(pane_key_quirks(&pane("reviewer", "claude", true)).ctrl_delete_as_meta_d);
        // `claude` typed in a shell pane, once the pane is named after it.
        assert!(pane_key_quirks(&pane("claude", "shell", true)).ctrl_delete_as_meta_d);
        // The stand-in shell after a failed resume, a plain shell, a pane
        // whose details have not arrived, and another agent.
        for p in [
            pane("powershell", "claude", false),
            pane("powershell", "shell", true),
            pane("terminal", "", false),
            pane("codex", "codex", true),
        ] {
            assert_eq!(pane_key_quirks(&p), KeyQuirks::default(), "{p:?}");
        }
    }
}
