//! YIEN's own icons, served next to gpui-kit's, which the title bar's window
//! buttons need.

use std::borrow::Cow;

use gpui_kit::assets::Assets as KitAssets;
use gpui_kit::{AssetSource, Result, SharedString};

use crate::model::AgentState;

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

const OWN: [(&str, &[u8]); 15] = [
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
    fn own_assets_are_listed_and_kit_icons_still_load() {
        let listed = YienAssets.list("yien/").unwrap();
        assert_eq!(listed.len(), OWN.len());
        assert!(YienAssets.load("icons/window-close.svg").unwrap().is_some());
    }
}
