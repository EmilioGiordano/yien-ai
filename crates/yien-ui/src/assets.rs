//! YIEN's own icons, served next to gpui-kit's, which the title bar's window
//! buttons need.

use std::borrow::Cow;
use std::time::Duration;

use gpui_kit::assets::Assets as KitAssets;
use gpui_kit::{AssetSource, Result, SharedString};

use crate::model::{AgentState, TabMark};

pub(crate) const MARK: &str = "yien/mark.svg";
pub(crate) const CHEVRON: &str = "yien/chevron.svg";
pub(crate) const PANEL_LEFT: &str = "yien/panel-left.svg";
pub(crate) const PANEL_RIGHT: &str = "yien/panel-right.svg";
pub(crate) const THEME: &str = "yien/theme.svg";
pub(crate) const BRANCH: &str = "yien/branch.svg";
pub(crate) const SPLIT_RIGHT: &str = "yien/split-right.svg";
pub(crate) const SPLIT_DOWN: &str = "yien/split-down.svg";
pub(crate) const PLUS: &str = "yien/plus.svg";
pub(crate) const CLOSE: &str = "yien/close.svg";
pub(crate) const FOLDER: &str = "yien/folder.svg";
pub(crate) const DOTS: &str = "yien/dots.svg";
pub(crate) const TERMINAL: &str = "yien/terminal.svg";

const OWN: [(&str, &[u8]); 18] = [
    (MARK, include_bytes!("../assets/mark.svg")),
    (CHEVRON, include_bytes!("../assets/chevron.svg")),
    (PANEL_LEFT, include_bytes!("../assets/panel-left.svg")),
    (PANEL_RIGHT, include_bytes!("../assets/panel-right.svg")),
    (THEME, include_bytes!("../assets/theme.svg")),
    (BRANCH, include_bytes!("../assets/branch.svg")),
    (SPLIT_RIGHT, include_bytes!("../assets/split-right.svg")),
    (SPLIT_DOWN, include_bytes!("../assets/split-down.svg")),
    (PLUS, include_bytes!("../assets/plus.svg")),
    (CLOSE, include_bytes!("../assets/close.svg")),
    (FOLDER, include_bytes!("../assets/folder.svg")),
    (DOTS, include_bytes!("../assets/dots.svg")),
    (TERMINAL, include_bytes!("../assets/terminal.svg")),
    (
        "yien/state-needs.svg",
        include_bytes!("../assets/state-needs.svg"),
    ),
    (
        "yien/state-error.svg",
        include_bytes!("../assets/state-error.svg"),
    ),
    (
        "yien/state-done.svg",
        include_bytes!("../assets/state-done.svg"),
    ),
    (
        "yien/state-working.svg",
        include_bytes!("../assets/state-working.svg"),
    ),
    (
        "yien/state-idle.svg",
        include_bytes!("../assets/state-idle.svg"),
    ),
];

/// The glyph for a state. Its shape tells the state apart, not only its colour.
pub(crate) fn state_icon(state: AgentState) -> &'static str {
    match state {
        AgentState::NeedsYou => "yien/state-needs.svg",
        AgentState::Error => "yien/state-error.svg",
        AgentState::Done => "yien/state-done.svg",
        AgentState::Working => "yien/state-working.svg",
        AgentState::Idle => "yien/state-idle.svg",
    }
}

pub(crate) const NEEDS_YOU_MARK: &str = "■";
pub(crate) const ERROR_MARK: &str = "!";
pub(crate) const DONE_MARK: &str = "●";
pub(crate) const IDLE_MARK: &str = "●";

/// The working spinner's frames, one per [`SPINNER_FRAME`].
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub(crate) const SPINNER_FRAME: Duration = Duration::from_millis(80);

/// The frame after `frame`, back to the first after the last.
pub(crate) fn next_frame(frame: usize) -> usize {
    (frame + 1) % SPINNER.len()
}

/// A glyph drawn in a tab row's mark slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MarkGlyph {
    pub(crate) text: &'static str,
    /// The text size at the prototype's 13 px window text.
    pub(crate) size: f32,
    pub(crate) bold: bool,
}

/// A tab row's mark in the mono font, `None` for a tab of shells only, whose
/// slot stays empty; `frame` is the spinner's while it works.
pub(crate) fn tab_mark_glyph(mark: TabMark, frame: usize) -> Option<MarkGlyph> {
    let glyph = |text, size, bold| Some(MarkGlyph { text, size, bold });
    match mark {
        TabMark::State(AgentState::NeedsYou) => glyph(NEEDS_YOU_MARK, 13.5, false),
        TabMark::State(AgentState::Error) => glyph(ERROR_MARK, 13.5, true),
        TabMark::State(AgentState::Done) => glyph(DONE_MARK, 11.0, false),
        TabMark::State(AgentState::Working) => glyph(SPINNER[frame % SPINNER.len()], 12.0, false),
        TabMark::State(AgentState::Idle) => glyph(IDLE_MARK, 11.0, false),
        TabMark::Shell => None,
    }
}

pub(crate) struct YienAssets;

impl AssetSource for YienAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match OWN.iter().find(|(name, _)| *name == path) {
            Some((_, bytes)) => Ok(Some(Cow::Borrowed(*bytes))),
            None => KitAssets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut listed = KitAssets.list(path)?;
        listed.extend(
            OWN.iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::new_static(name)),
        );
        Ok(listed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_its_own_embedded_glyph() {
        let states = [
            AgentState::NeedsYou,
            AgentState::Error,
            AgentState::Done,
            AgentState::Working,
            AgentState::Idle,
        ];
        let paths: std::collections::HashSet<_> = states.iter().map(|s| state_icon(*s)).collect();
        for path in &paths {
            let bytes = YienAssets.load(path).unwrap().unwrap();
            assert!(bytes.starts_with(b"<svg"), "{path}");
        }
        assert_eq!(paths.len(), states.len());
    }

    fn tab_mark_text(mark: TabMark, frame: usize) -> &'static str {
        tab_mark_glyph(mark, frame).unwrap().text
    }

    #[test]
    fn tab_marks_are_the_canvas_glyphs_at_their_sizes() {
        let glyph = |state| tab_mark_glyph(TabMark::State(state), 0).unwrap();
        let want = [
            (AgentState::NeedsYou, "■", 13.5, false),
            (AgentState::Error, "!", 13.5, true),
            (AgentState::Done, "●", 11.0, false),
            (AgentState::Working, "⠋", 12.0, false),
            (AgentState::Idle, "●", 11.0, false),
        ];
        for (state, text, size, bold) in want {
            let got = glyph(state);
            assert_eq!(
                (got.text, got.size, got.bold),
                (text, size, bold),
                "{state:?}"
            );
            assert!(got.size <= 14.0, "{state:?} fits its 14 px slot");
        }
    }

    #[test]
    fn a_tab_of_shells_only_has_no_mark() {
        assert_eq!(tab_mark_glyph(TabMark::Shell, 0), None);
        assert_eq!(tab_mark_glyph(TabMark::Shell, 7), None);
    }

    #[test]
    fn rnf_17_every_tab_mark_differs_by_shape_or_colour_in_both_themes() {
        use crate::theme::{Theme, ThemeKind};
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            let look = |s| {
                let mark = TabMark::State(s);
                (tab_mark_text(mark, 0), t.tab_mark_color(mark))
            };
            for (i, a) in AgentState::BY_URGENCY.into_iter().enumerate() {
                for b in AgentState::BY_URGENCY.into_iter().skip(i + 1) {
                    assert_ne!(look(a), look(b), "{kind:?}: {a:?} and {b:?}");
                }
            }
        }
        let working = TabMark::State(AgentState::Working);
        for state in AgentState::BY_URGENCY {
            let mark = TabMark::State(state);
            if mark != working {
                let text = tab_mark_text(mark, 0);
                assert!(!SPINNER.contains(&text), "{state:?} looks like working");
            }
        }
    }

    #[test]
    fn the_spinner_runs_its_ten_frames_and_wraps() {
        assert_eq!(SPINNER.concat(), "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");
        assert_eq!(SPINNER_FRAME, Duration::from_millis(80));
        let working = TabMark::State(AgentState::Working);
        let mut frame = 0;
        let mut seen = Vec::new();
        for _ in 0..SPINNER.len() {
            seen.push(tab_mark_text(working, frame));
            frame = next_frame(frame);
        }
        assert_eq!(seen, SPINNER);
        assert_eq!(frame, 0, "the last frame wraps to the first");
        assert_eq!(tab_mark_text(working, SPINNER.len() + 3), SPINNER[3]);
        let idle = TabMark::State(AgentState::Idle);
        assert_eq!(tab_mark_text(idle, 4), tab_mark_text(idle, 0));
    }

    #[test]
    fn own_assets_are_listed_and_kit_icons_still_load() {
        let listed = YienAssets.list("yien/").unwrap();
        assert_eq!(listed.len(), OWN.len());
        assert!(YienAssets.load("icons/window-close.svg").unwrap().is_some());
    }
}
