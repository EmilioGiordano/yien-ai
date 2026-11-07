//! The two palettes of the prototype: sumi (ink, dark) and washi (paper, light).

use gpui_kit::component::{Theme as KitTheme, ThemeMode};
use gpui_kit::{App, Hsla, Rgba, rgb, rgba};

use crate::git::FileMark;
use crate::model::AgentState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ThemeKind {
    #[default]
    Sumi,
    Washi,
}

impl ThemeKind {
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Sumi => Self::Washi,
            Self::Washi => Self::Sumi,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Theme {
    pub(crate) kind: ThemeKind,
    pub(crate) bg: Rgba,
    pub(crate) panel: Rgba,
    pub(crate) raised: Rgba,
    pub(crate) hover: Rgba,
    pub(crate) line: Rgba,
    pub(crate) text: Rgba,
    pub(crate) muted: Rgba,
    pub(crate) faint: Rgba,
    pub(crate) shu: Rgba,
    pub(crate) shu_wash: Rgba,
    pub(crate) ai: Rgba,
    pub(crate) ochre: Rgba,
    pub(crate) term_bg: Rgba,
    pub(crate) term_raised: Rgba,
    pub(crate) term_line: Rgba,
    pub(crate) term_strong: Rgba,
    pub(crate) term_text: Rgba,
    pub(crate) term_muted: Rgba,
    pub(crate) term_faint: Rgba,
}

impl Theme {
    pub(crate) fn of(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Sumi => Self::sumi(),
            ThemeKind::Washi => Self::washi(),
        }
    }

    fn sumi() -> Self {
        Self {
            kind: ThemeKind::Sumi,
            bg: rgb(0x0d0c0b),
            panel: rgb(0x141311),
            raised: rgb(0x1c1a17),
            hover: rgb(0x23211d),
            line: rgb(0x2e2b27),
            text: rgb(0xe9e6dd),
            muted: rgb(0xa19d93),
            faint: rgb(0x8a867c),
            shu: rgb(0xe8333c),
            shu_wash: rgba(0xe8333c1f),
            ai: rgb(0x8fa8dc),
            ochre: rgb(0xd6a24c),
            term_bg: rgb(0x0d0c0b),
            term_raised: rgb(0x1c1a17),
            term_line: rgb(0x2e2b27),
            term_strong: rgb(0x55514a),
            term_text: rgb(0xe9e6dd),
            term_muted: rgb(0xa19d93),
            term_faint: rgb(0x7d796f),
        }
    }

    fn washi() -> Self {
        Self {
            kind: ThemeKind::Washi,
            bg: rgb(0xf7f6f0),
            panel: rgb(0xefede5),
            raised: rgb(0xfbfaf6),
            hover: rgb(0xe6e3d8),
            line: rgb(0xd6d4ca),
            text: rgb(0x23211e),
            muted: rgb(0x5c5952),
            faint: rgb(0x66635b),
            shu: rgb(0xd11f28),
            shu_wash: rgba(0xb3121b14),
            ai: rgb(0x2f4f8f),
            ochre: rgb(0x8a5a12),
            term_bg: rgb(0xfbfaf6),
            term_raised: rgb(0xf1efe7),
            term_line: rgb(0xd6d4ca),
            term_strong: rgb(0xa9a69c),
            term_text: rgb(0x23211e),
            term_muted: rgb(0x5c5952),
            term_faint: rgb(0x6f6c64),
        }
    }

    /// The glyph colour of a state. Red is for "needs you" and nothing else (RN-01).
    pub(crate) fn state_color(&self, state: AgentState) -> Rgba {
        match state {
            AgentState::NeedsYou => self.shu,
            AgentState::Error => self.ochre,
            AgentState::Done | AgentState::Working => self.ai,
            AgentState::Idle => self.faint,
        }
    }

    /// The colour of a changed file's letter. Never red, conflicts included (RN-01).
    pub(crate) fn mark_color(&self, mark: FileMark) -> Rgba {
        match mark {
            FileMark::Modified => self.muted,
            FileMark::Added | FileMark::Renamed | FileMark::Copied => self.ai,
            FileMark::Deleted | FileMark::Conflicted => self.ochre,
            FileMark::Untracked => self.faint,
        }
    }

    /// The colour of a state's words in a tab row.
    pub(crate) fn label_color(&self, state: AgentState) -> Rgba {
        match state {
            AgentState::NeedsYou => self.shu,
            AgentState::Error => self.ochre,
            AgentState::Done => self.ai,
            AgentState::Working | AgentState::Idle => self.muted,
        }
    }

    /// A pane's border width and colour: red and thicker while its agent
    /// needs the user, stronger than its neighbours' when it has the focus
    /// (RF-PAN-17).
    pub(crate) fn pane_border(&self, focused: bool, needs_you: bool) -> (f32, Rgba) {
        if needs_you {
            (2.0, self.shu)
        } else if focused {
            (1.0, self.term_strong)
        } else {
            (1.0, self.term_line)
        }
    }

    /// The red bar at the left of a tab row, drawn whether or not the tab
    /// is selected, only while one of its panes needs the user (RF-ATN-15).
    pub(crate) fn tab_row_bar(&self, state: AgentState) -> Option<Rgba> {
        state.needs_you().then_some(self.shu)
    }
}

/// Points gpui-component, which paints the title bar, its window buttons and
/// the palette's text input, at the same palette.
pub(crate) fn sync_component_theme(theme: &Theme, cx: &mut App) {
    let mode = match theme.kind {
        ThemeKind::Sumi => ThemeMode::Dark,
        ThemeKind::Washi => ThemeMode::Light,
    };
    KitTheme::change(mode, None, cx);
    let c = |color: Rgba| -> Hsla { color.into() };
    KitTheme::update(cx, |kit| {
        let colors = &mut kit.colors;
        colors.background = c(theme.bg);
        colors.foreground = c(theme.text);
        colors.muted_foreground = c(theme.muted);
        colors.border = c(theme.line);
        colors.title_bar = c(theme.panel);
        colors.title_bar_border = c(theme.line);
        colors.secondary_foreground = c(theme.muted);
        colors.secondary_hover = c(theme.hover);
        colors.secondary_active = c(theme.raised);
        // The close button hovers in `danger`; red belongs to "needs you" (RN-01).
        colors.danger = c(theme.ochre);
        colors.danger_hover = c(theme.ochre);
        colors.danger_active = c(theme.ochre);
        colors.danger_foreground = c(theme.bg);
        colors.caret = c(theme.text);
        let mut selection = c(theme.ai);
        selection.a = 0.3;
        colors.selection = selection;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: Rgba) -> f32 {
        let lin = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn rnf_18_neutral_and_accent_text_meets_aa_in_both_themes() {
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            let pairs = [
                ("text on panel", t.text, t.panel),
                ("text on raised", t.text, t.raised),
                ("muted on panel", t.muted, t.panel),
                ("muted on raised", t.muted, t.raised),
                ("faint on panel", t.faint, t.panel),
                ("faint on raised", t.faint, t.raised),
                ("faint on bg", t.faint, t.bg),
                ("ai on raised", t.ai, t.raised),
                ("ochre on raised", t.ochre, t.raised),
                ("term text on term bg", t.term_text, t.term_bg),
                ("term muted on term raised", t.term_muted, t.term_raised),
                ("term faint on term bg", t.term_faint, t.term_bg),
            ];
            for (name, fg, bg) in pairs {
                let ratio = contrast(fg, bg);
                assert!(ratio >= 4.5, "{kind:?}: {name} is {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn rn_01_only_needs_you_is_red() {
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            for state in [
                AgentState::Error,
                AgentState::Done,
                AgentState::Working,
                AgentState::Idle,
            ] {
                assert_ne!(t.state_color(state), t.shu, "{state:?}");
                assert_ne!(t.label_color(state), t.shu, "{state:?}");
            }
            assert_eq!(t.state_color(AgentState::NeedsYou), t.shu);
            assert_eq!(t.label_color(AgentState::NeedsYou), t.shu);
        }
    }

    #[test]
    fn rf_pan_17_the_focused_pane_stands_out_from_the_others() {
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            let (_, focused) = t.pane_border(true, false);
            let (_, other) = t.pane_border(false, false);
            assert_ne!(focused, other, "{kind:?}");
            assert_ne!(focused, t.shu, "{kind:?}: focus is not red (RN-01)");
            assert!(
                contrast(focused, t.term_bg) > contrast(other, t.term_bg),
                "{kind:?}: the focused border is the stronger one"
            );
            assert_eq!(t.pane_border(true, true), (2.0, t.shu), "{kind:?}");
        }
    }

    #[test]
    fn toggling_switches_between_the_two_palettes() {
        assert_eq!(ThemeKind::default(), ThemeKind::Sumi);
        assert_eq!(ThemeKind::Sumi.toggled(), ThemeKind::Washi);
        assert_eq!(ThemeKind::Washi.toggled().toggled(), ThemeKind::Washi);
        assert_eq!(Theme::of(ThemeKind::Washi).kind, ThemeKind::Washi);
        assert_ne!(
            Theme::of(ThemeKind::Sumi).bg,
            Theme::of(ThemeKind::Washi).bg
        );
    }
}
