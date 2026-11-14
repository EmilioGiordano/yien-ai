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
pub(crate) const DONE_MARK: &str = "✓";
pub(crate) const IDLE_MARK: &str = "○";
pub(crate) const SHELL_MARK: &str = ">";

/// The working spinner's frames, one per [`SPINNER_FRAME`].
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub(crate) const SPINNER_FRAME: Duration = Duration::from_millis(80);

/// The frame after `frame`, back to the first after the last.
pub(crate) fn next_frame(frame: usize) -> usize {
    (frame + 1) % SPINNER.len()
}

/// A tab row's mark in the mono font; `frame` is the spinner's while it works.
pub(crate) fn tab_mark_text(mark: TabMark, frame: usize) -> &'static str {
    match mark {
        TabMark::State(AgentState::NeedsYou) => NEEDS_YOU_MARK,
        TabMark::State(AgentState::Error) => ERROR_MARK,
        TabMark::State(AgentState::Done) => DONE_MARK,
        TabMark::State(AgentState::Working) => SPINNER[frame % SPINNER.len()],
        TabMark::State(AgentState::Idle) => IDLE_MARK,
        TabMark::Shell => SHELL_MARK,
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

    #[test]
    fn rnf_17_every_tab_mark_has_its_own_shape() {
        let marks = [
            TabMark::State(AgentState::NeedsYou),
            TabMark::State(AgentState::Error),
            TabMark::State(AgentState::Done),
            TabMark::State(AgentState::Working),
            TabMark::State(AgentState::Idle),
            TabMark::Shell,
        ];
        let texts: std::collections::HashSet<_> =
            marks.iter().map(|m| tab_mark_text(*m, 0)).collect();
        assert_eq!(texts.len(), marks.len());
        assert_eq!(tab_mark_text(marks[0], 0), "■");
        assert_eq!(tab_mark_text(marks[2], 0), "✓");
        assert_eq!(tab_mark_text(marks[5], 0), ">");
        for mark in marks.iter().filter(|m| **m != marks[3]) {
            let text = tab_mark_text(*mark, 0);
            assert!(!SPINNER.contains(&text), "{mark:?} looks like working");
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
