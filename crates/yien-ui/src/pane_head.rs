//! Which pane head pieces to draw at a pane width (RF-PAN-12, RF-PAN-16).
//! Flex ellipsizes the text and keeps the buttons whole; this only drops the
//! pieces that would not fit at their full width.

use yien_proto::SHELL_PROFILE;

use crate::model::PaneView;

/// The head's own horizontal padding, each side.
pub(crate) const PADDING: f32 = 8.0;
pub(crate) const GAP: f32 = 8.0;
pub(crate) const GLYPH: f32 = 14.0;
pub(crate) const BUTTON: f32 = 20.0;
/// The text size of the name, profile and state label, and of the folder.
pub(crate) const TEXT_SIZE: f32 = 12.5;
pub(crate) const FOLDER_SIZE: f32 = 12.0;
/// A generous average advance, in ems, so a piece is dropped rather than
/// ellipsized when the guess is off.
const EM: f32 = 0.6;
/// What a shell's name keeps before the pieces after it drop: a name is
/// "only if room", but a few letters still tell `pwsh` from `bash`.
const SHELL_NAME_CHARS: usize = 4;

/// Which program a pane head names (RF-PAN-12): the agent profile's id while
/// that agent's own program runs there, else the pane's name. A stand-in
/// shell after a failed resume, or a pane whose details have not arrived, is
/// not claimed as the agent.
///
/// ```ignore
/// assert_eq!(program_tag(&claude_pane), "claude");
/// ```
pub(crate) fn program_tag(pane: &PaneView) -> &str {
    if runs_agent(pane) {
        pane.profile_id.as_str()
    } else {
        &pane.name
    }
}

/// Whether an agent profile's own program runs in `pane`.
pub(crate) fn runs_agent(pane: &PaneView) -> bool {
    let id = pane.profile_id.as_str();
    !id.is_empty() && id != SHELL_PROFILE && pane.runs_session
}

/// The text a pane head may show, as it would read in full.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeadText<'a> {
    /// The pane's name: the user's or its program's.
    pub name: &'a str,
    /// The profile's display name: `Claude Code`, `Shell`.
    pub profile: &'a str,
    /// What [`program_tag`] says.
    pub tag: &'a str,
    /// Whether `tag` names an agent; else it is a shell or other program.
    pub agent: bool,
    /// The folder up to its last name, and that last name.
    pub folder_lead: &'a str,
    pub folder_last: &'a str,
    /// The state's word: `Working`.
    pub state: &'a str,
    pub signal_lost: Option<&'a str>,
}

impl HeadText<'_> {
    /// Whether the name is shown apart from the tag: a renamed agent pane.
    pub(crate) fn separate_name(&self) -> bool {
        !self.name.eq_ignore_ascii_case(self.tag)
    }
}

/// What a pane head draws. The glyph and the program are always drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneHeadLayout {
    /// The profile's display name rather than only the tag.
    pub profile: bool,
    /// The state's word beside the glyph.
    pub state_label: bool,
    pub split_buttons: bool,
    pub menu_button: bool,
    /// The folder before its last name, and the last name.
    pub folder_lead: bool,
    pub folder_last: bool,
    pub signal_lost: bool,
    /// The pane's own name, when it is not the tag.
    pub name: bool,
    pub close_button: bool,
}

impl PaneHeadLayout {
    /// Every piece, as a wide pane draws it.
    pub(crate) const FULL: Self = Self {
        profile: true,
        state_label: true,
        split_buttons: true,
        menu_button: true,
        folder_lead: true,
        folder_last: true,
        signal_lost: true,
        name: true,
        close_button: true,
    };

    /// The layout for a head `width` px wide, text scaled by `scale`
    /// (`ui.font.size` over 13). Pieces go in this order: the profile's
    /// display name, the state word, the split buttons, the menu button,
    /// the folder's lead, its last name, "signal lost", a renamed pane's
    /// name, the close button. The glyph and the program stay.
    pub(crate) fn at(width: f32, scale: f32, text: &HeadText<'_>) -> Self {
        let mut layout = Self::FULL;
        let drops: [fn(&mut Self); 9] = [
            |l| l.profile = false,
            |l| l.state_label = false,
            |l| l.split_buttons = false,
            |l| l.menu_button = false,
            |l| l.folder_lead = false,
            |l| l.folder_last = false,
            |l| l.signal_lost = false,
            |l| l.name = false,
            |l| l.close_button = false,
        ];
        for drop in drops {
            if layout.need(scale, text) <= width {
                return layout;
            }
            drop(&mut layout);
        }
        layout
    }

    /// The width this layout takes with each piece at its full estimated
    /// width, except a shell's name, which counts only its first letters.
    pub(crate) fn need(&self, scale: f32, text: &HeadText<'_>) -> f32 {
        let words = |s: &str| chars(s, TEXT_SIZE, scale);
        let mut pieces = vec![GLYPH];
        let separate = text.separate_name();
        if separate && self.name {
            pieces.push(words(text.name));
        }
        let who = if separate && self.profile {
            words(text.profile)
        } else if text.agent {
            words(text.tag)
        } else {
            let shown = text.tag.chars().count().min(SHELL_NAME_CHARS);
            shown as f32 * EM * TEXT_SIZE * scale
        };
        pieces.push(who);
        if !separate && self.profile {
            pieces.push(words(text.profile));
        }
        if self.folder_last {
            let lead = if self.folder_lead {
                chars(text.folder_lead, FOLDER_SIZE, scale)
            } else {
                0.0
            };
            pieces.push(lead + chars(text.folder_last, FOLDER_SIZE, scale));
        }
        // The spacer that pushes what follows to the right end.
        pieces.push(0.0);
        if self.state_label {
            pieces.push(words(text.state));
        }
        if let (true, Some(lost)) = (self.signal_lost, text.signal_lost) {
            pieces.push(words(lost));
        }
        let buttons = 2 * usize::from(self.split_buttons)
            + usize::from(self.menu_button)
            + usize::from(self.close_button);
        pieces.extend(std::iter::repeat_n(BUTTON, buttons));
        let gaps = (pieces.len() - 1) as f32 * GAP;
        2.0 * PADDING + gaps + pieces.iter().sum::<f32>()
    }
}

/// The estimated width of `s` at `size` px, scaled.
fn chars(s: &str, size: f32, scale: f32) -> f32 {
    s.chars().count() as f32 * EM * size * scale
}

#[cfg(test)]
mod tests {
    use yien_proto::{PaneId, ProfileId};

    use super::{BUTTON, GAP, GLYPH, HeadText, PADDING, PaneHeadLayout, program_tag, runs_agent};
    use crate::model::{AgentState, PaneView};

    const BIGGEST: f32 = 20.0 / 13.0;
    const SCALES: [f32; 4] = [1.0, 1.25, 1.5, BIGGEST];

    fn claude() -> HeadText<'static> {
        HeadText {
            name: "claude",
            profile: "Claude Code",
            tag: "claude",
            agent: true,
            folder_lead: "~/Code/",
            folder_last: "yien-ai",
            state: "Working",
            signal_lost: None,
        }
    }

    fn renamed() -> HeadText<'static> {
        HeadText {
            name: "reviewer",
            ..claude()
        }
    }

    fn shell() -> HeadText<'static> {
        HeadText {
            name: "powershell",
            profile: "Shell",
            tag: "powershell",
            agent: false,
            state: "Idle",
            ..claude()
        }
    }

    fn lost() -> HeadText<'static> {
        HeadText {
            signal_lost: Some("signal lost"),
            ..renamed()
        }
    }

    fn pieces(l: &PaneHeadLayout) -> [bool; 9] {
        [
            l.profile,
            l.state_label,
            l.split_buttons,
            l.menu_button,
            l.folder_lead,
            l.folder_last,
            l.signal_lost,
            l.name,
            l.close_button,
        ]
    }

    /// Each width from wide to narrow, at each scale, for each kind of head.
    fn sweep(mut check: impl FnMut(f32, f32, &HeadText<'_>, PaneHeadLayout)) {
        for text in [claude(), renamed(), shell(), lost()] {
            for scale in SCALES {
                let mut width = 1600.0;
                while width >= 10.0 {
                    check(width, scale, &text, PaneHeadLayout::at(width, scale, &text));
                    width -= 2.0;
                }
            }
        }
    }

    #[test]
    fn rf_pan_12_a_wide_pane_keeps_every_piece() {
        for text in [claude(), renamed(), shell(), lost()] {
            for scale in SCALES {
                assert_eq!(
                    PaneHeadLayout::at(900.0, scale, &text),
                    PaneHeadLayout::FULL
                );
            }
        }
    }

    #[test]
    fn rnf_19_narrowing_drops_pieces_in_order_and_never_brings_them_back() {
        let mut last: Option<(f32, PaneHeadLayout)> = None;
        sweep(|width, scale, _, now| {
            if let Some((was, before)) = last.filter(|(w, _)| *w > width) {
                for (b, a) in pieces(&before).into_iter().zip(pieces(&now)) {
                    assert!(
                        b || !a,
                        "a piece came back at {width} (from {was}), {scale}"
                    );
                }
            }
            let kept = pieces(&now);
            // A piece that is kept keeps every piece after it in the order.
            for at in 1..kept.len() {
                assert!(!kept[at - 1] || kept[at], "{kept:?} at {width}, {scale}");
            }
            last = Some((width, now));
        });
    }

    #[test]
    fn rnf_19_a_kept_button_is_never_cut_half_way() {
        sweep(|width, scale, text, now| {
            let any_button = now.split_buttons || now.menu_button || now.close_button;
            if any_button {
                assert!(
                    now.need(scale, text) <= width,
                    "{now:?} at {width}, {scale}"
                );
            }
        });
    }

    #[test]
    fn rf_pan_12_the_agent_outlives_the_folder_and_the_close_button() {
        sweep(|width, scale, text, now| {
            if text.agent && (now.folder_last || now.close_button) {
                // The need counts an agent's tag at its full width.
                assert!(
                    now.need(scale, text) <= width,
                    "{now:?} at {width}, {scale}"
                );
            }
        });
    }

    #[test]
    fn rf_pan_16_the_folder_keeps_its_last_name_after_the_buttons_go() {
        let text = claude();
        let narrow = (100..900)
            .map(|w| PaneHeadLayout::at(w as f32, 1.0, &text))
            .find(|l| l.folder_last && !l.folder_lead && !l.menu_button);
        assert!(narrow.is_some(), "no width shows only the last folder");
    }

    #[test]
    fn rf_pan_20_the_close_button_stays_while_glyph_and_program_fit() {
        for scale in SCALES {
            let text = claude();
            let tag = 6.0 * 0.6 * 12.5 * scale;
            // Glyph, tag, the spacer and the button: three gaps.
            let width = 2.0 * PADDING + GLYPH + tag + 3.0 * GAP + BUTTON;
            assert!(PaneHeadLayout::at(width + 1.0, scale, &text).close_button);
            assert!(!PaneHeadLayout::at(width - 1.0, scale, &text).close_button);
        }
    }

    #[test]
    fn rf_pan_12_a_shell_name_gives_way_before_the_close_button() {
        let text = shell();
        let width = 2.0 * PADDING + GLYPH + 3.0 * GAP + BUTTON + 4.0 * 0.6 * 12.5 + 1.0;
        assert!(PaneHeadLayout::at(width, 1.0, &text).close_button);
    }

    fn pane(profile: &str, runs_session: bool) -> PaneView {
        PaneView {
            id: PaneId::new(1),
            name: "powershell".into(),
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
    fn rf_pan_12_an_agent_pane_is_named_by_its_agent() {
        assert_eq!(program_tag(&pane("claude", true)), "claude");
        assert_eq!(program_tag(&pane("codex", true)), "codex");
        assert_eq!(program_tag(&pane("opencode", true)), "opencode");
        assert!(runs_agent(&pane("claude", true)));
    }

    #[test]
    fn rf_pan_12_a_shell_a_stand_in_shell_or_an_unknown_pane_shows_its_name() {
        assert_eq!(program_tag(&pane("shell", true)), "powershell");
        assert_eq!(program_tag(&pane("claude", false)), "powershell");
        assert_eq!(program_tag(&pane("", true)), "powershell");
        for (profile, runs) in [("shell", true), ("claude", false), ("", true)] {
            assert!(!runs_agent(&pane(profile, runs)), "{profile} {runs}");
        }
    }
}
