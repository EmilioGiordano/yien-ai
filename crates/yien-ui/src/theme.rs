//! The two palettes of the prototype: sumi (ink, dark) and washi (paper, light).

use gpui_kit::component::{Theme as KitTheme, ThemeMode};
use gpui_kit::{App, Hsla, Pixels, Rgba, px, rgb, rgba};
use yien_proto::TerminalPalette;
use yien_proto::file_preview::SyntaxKind;

use crate::git::FileMark;
use crate::model::{AgentState, TabMark};

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

    /// The palette the core resolves terminal colours with under this theme.
    pub(crate) fn terminal_palette(self) -> TerminalPalette {
        match self {
            Self::Sumi => TerminalPalette::Sumi,
            Self::Washi => TerminalPalette::Washi,
        }
    }
}

/// The window text size the prototype's sizes are drawn at.
pub(crate) const BASE_UI_SIZE: f32 = 13.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Theme {
    pub(crate) kind: ThemeKind,
    /// The configured window text size (`ui.font.size`); every text size
    /// is scaled by it over [`BASE_UI_SIZE`].
    pub(crate) ui_size: f32,
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
    /// A tab row's state words other than the red ones.
    pub(crate) mark_dim: Rgba,
    /// A tab row's mark and words while it needs the user or failed (RN-01).
    pub(crate) mark_red: Rgba,
    /// A tab row's state words while it needs the user or failed: the mark
    /// red, lighter where that red is below 4.5:1 as text (RN-01, RNF-18).
    pub(crate) word_red: Rgba,
    /// A tab row's dot once its agent finished a turn.
    pub(crate) mark_done: Rgba,
    /// A tab row's dot while its agent waits with nothing to do.
    pub(crate) mark_idle: Rgba,
    pub(crate) term_bg: Rgba,
    pub(crate) term_raised: Rgba,
    pub(crate) term_line: Rgba,
    pub(crate) term_strong: Rgba,
    pub(crate) term_text: Rgba,
    pub(crate) term_muted: Rgba,
    pub(crate) term_faint: Rgba,
    /// Behind selected terminal cells, which keep their own text colour.
    pub(crate) term_selection: Rgba,
    /// File preview syntax token colours (RF-CTX-02, never red per RN-01).
    pub(crate) syn_kw: Rgba,
    pub(crate) syn_ty: Rgba,
    pub(crate) syn_fn: Rgba,
    pub(crate) syn_str: Rgba,
    pub(crate) syn_num: Rgba,
    pub(crate) syn_com: Rgba,
    pub(crate) syn_mac: Rgba,
}

impl Theme {
    pub(crate) fn of(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Sumi => Self::sumi(),
            ThemeKind::Washi => Self::washi(),
        }
    }

    /// The same palette with the window's text at `size`.
    pub(crate) fn with_ui_size(self, size: f32) -> Self {
        Self {
            ui_size: size,
            ..self
        }
    }

    /// A text size of the prototype, `base` at 13 px, at the configured size.
    pub(crate) fn text(&self, base: f32) -> Pixels {
        px(base * self.ui_size / BASE_UI_SIZE)
    }

    fn sumi() -> Self {
        Self {
            kind: ThemeKind::Sumi,
            ui_size: BASE_UI_SIZE,
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
            mark_dim: rgb(0xa19d93),
            mark_red: rgb(0xe8333c),
            word_red: rgb(0xf0545c),
            mark_done: rgb(0x6fd39a),
            mark_idle: rgb(0xcc7a2e),
            term_bg: rgb(0x0d0c0b),
            term_raised: rgb(0x1c1a17),
            term_line: rgb(0x2e2b27),
            term_strong: rgb(0x55514a),
            term_text: rgb(0xe9e6dd),
            term_muted: rgb(0xa19d93),
            term_faint: rgb(0x7d796f),
            term_selection: rgb(0x55514a),
            syn_kw: rgb(0xc08fb5),
            syn_ty: rgb(0x7fb4b0),
            syn_fn: rgb(0x8fa8dc),
            syn_str: rgb(0x8fb573),
            syn_num: rgb(0xd6a24c),
            syn_com: rgb(0x7d796f),
            syn_mac: rgb(0xd6a24c),
        }
    }

    fn washi() -> Self {
        Self {
            kind: ThemeKind::Washi,
            ui_size: BASE_UI_SIZE,
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
            mark_dim: rgb(0x5f5c55),
            mark_red: rgb(0xb3121b),
            word_red: rgb(0xb3121b),
            mark_done: rgb(0x22633e),
            mark_idle: rgb(0xbc6a1e),
            term_bg: rgb(0xfbfaf6),
            term_raised: rgb(0xf1efe7),
            term_line: rgb(0xd6d4ca),
            term_strong: rgb(0xa9a69c),
            term_text: rgb(0x23211e),
            term_muted: rgb(0x5c5952),
            term_faint: rgb(0x6f6c64),
            // --ai-wash at 14 % over --term-bg: `term_strong` is too dark for
            // the coloured text selected cells keep.
            term_selection: rgb(0xdee2e8),
            syn_kw: rgb(0x8a3d7c),
            syn_ty: rgb(0x1f6b66),
            syn_fn: rgb(0x2f4f8f),
            syn_str: rgb(0x3e6b24),
            syn_num: rgb(0x8a5a12),
            syn_com: rgb(0x6f6c64),
            syn_mac: rgb(0x8a5a12),
        }
    }

    /// The colour of a pane head's state word: red for "needs you" and an
    /// error, told apart by the mark's shape (RN-01), muted otherwise.
    pub(crate) fn pane_word_color(&self, state: AgentState) -> Rgba {
        match state {
            AgentState::NeedsYou | AgentState::Error => self.word_red,
            _ => self.term_muted,
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

    /// The colour of a tab row's mark: red for "needs you" and an error,
    /// told apart by shape (RN-01), green when done, orange when idle,
    /// indigo (`ai`) while working.
    pub(crate) fn tab_mark_color(&self, mark: TabMark) -> Rgba {
        match mark {
            TabMark::State(AgentState::NeedsYou | AgentState::Error) => self.mark_red,
            TabMark::State(AgentState::Done) => self.mark_done,
            TabMark::State(AgentState::Idle) => self.mark_idle,
            TabMark::State(AgentState::Working) => self.ai,
            TabMark::Shell => self.mark_dim,
        }
    }

    /// The colour of a tab row's state words: red for "needs you" and an
    /// error (RN-01), dim otherwise; the dots' colours are too faint for text.
    pub(crate) fn tab_word_color(&self, state: AgentState) -> Rgba {
        match state {
            AgentState::NeedsYou | AgentState::Error => self.word_red,
            _ => self.mark_dim,
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

    /// The ring around the region that holds the keyboard (RF-NAV-08).
    /// Indigo, never red: red means an agent needs the user or failed (RN-01).
    pub(crate) fn focus_ring(&self) -> Rgba {
        self.ai
    }

    /// A file pane's scrollbar thumb: faint at rest, stronger while hovered
    /// or dragged. Neutral, never red (RN-01).
    pub(crate) fn scroll_thumb(&self, strong: bool) -> Rgba {
        if strong {
            self.term_strong
        } else {
            self.term_line
        }
    }

    /// The syntax token colour for a previewed file's span (RF-CTX-02, RN-01).
    pub(crate) fn syntax_color(&self, kind: SyntaxKind) -> Rgba {
        match kind {
            SyntaxKind::Plain => self.term_text,
            SyntaxKind::Keyword => self.syn_kw,
            SyntaxKind::Type => self.syn_ty,
            SyntaxKind::Function => self.syn_fn,
            SyntaxKind::String => self.syn_str,
            SyntaxKind::Number => self.syn_num,
            SyntaxKind::Comment => self.syn_com,
            SyntaxKind::Macro => self.syn_mac,
        }
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
        // The close button hovers in `danger`; red belongs to agent states (RN-01).
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
                ("dim mark on panel", t.mark_dim, t.panel),
                ("dim mark on raised", t.mark_dim, t.raised),
                ("dim mark on hover", t.mark_dim, t.hover),
                ("red words on panel", t.word_red, t.panel),
                ("red words on raised", t.word_red, t.raised),
                ("red words on hover", t.word_red, t.hover),
                ("red words on term bg", t.word_red, t.term_bg),
                ("red words on term raised", t.word_red, t.term_raised),
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
    fn rf_cfg_02_each_theme_picks_its_terminal_palette() {
        assert_eq!(ThemeKind::Sumi.terminal_palette(), TerminalPalette::Sumi);
        assert_eq!(ThemeKind::Washi.terminal_palette(), TerminalPalette::Washi);
        assert_eq!(
            ThemeKind::default().terminal_palette(),
            TerminalPalette::default(),
            "the core starts with the palette of the window's first theme"
        );
    }

    #[test]
    fn rnf_18_selected_terminal_text_meets_aa_in_both_themes() {
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            let ratio = contrast(t.term_text, t.term_selection);
            assert!(ratio >= 4.5, "{kind:?}: selected text is {ratio:.2}:1");
        }
        let sumi = Theme::of(ThemeKind::Sumi);
        assert_eq!(sumi.term_selection, sumi.term_strong, "sumi is unchanged");
    }

    #[test]
    fn rnf_18_selected_washi_colours_stay_readable() {
        // `yien_vt::Palette::washi()`, ANSI 1-6 and 9-14; yien-ui cannot
        // depend on yien-vt.
        let colours = [
            0xd11f28, 0x3e6b24, 0x8a5a12, 0x2f4f8f, 0x8a3d7c, 0x1f6b66, 0xd9444a, 0x5a8a34,
            0xa86f12, 0x4a6cb5, 0xa8559a, 0x2f8781,
        ];
        let t = Theme::of(ThemeKind::Washi);
        for hex in colours {
            let ratio = contrast(rgb(hex), t.term_selection);
            assert!(ratio >= 3.0, "{hex:06x} on the selection is {ratio:.2}:1");
        }
        let ratio = contrast(t.term_selection, t.term_bg);
        assert!(ratio > 1.2, "the selection shows on the grid: {ratio:.2}:1");
    }

    #[test]
    fn rn_01_only_needs_you_and_error_are_red_and_only_needs_you_has_bar_and_border() {
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            for state in AgentState::BY_URGENCY {
                let red = matches!(state, AgentState::NeedsYou | AgentState::Error);
                let mark = t.tab_mark_color(TabMark::State(state));
                assert_eq!(mark == t.mark_red, red, "{kind:?} {state:?} mark");
                assert_eq!(t.tab_word_color(state) == t.word_red, red, "{state:?}");
                if !red {
                    assert_ne!(mark, t.shu, "{kind:?} {state:?}");
                }
                let bar = t.tab_row_bar(state);
                assert_eq!(bar.is_some(), state == AgentState::NeedsYou, "{state:?}");
                let word = t.pane_word_color(state);
                assert_eq!(word == t.word_red, red, "{kind:?} {state:?} pane word");
                if !red {
                    assert_ne!(word, t.shu, "{kind:?} {state:?}");
                }
                for focused in [false, true] {
                    let (_, border) = t.pane_border(focused, state.needs_you());
                    let red_border = border == t.shu;
                    assert_eq!(red_border, state == AgentState::NeedsYou, "{state:?}");
                }
            }
            assert_ne!(t.tab_mark_color(TabMark::Shell), t.mark_red);
            assert_ne!(t.tab_mark_color(TabMark::Shell), t.shu);
        }
    }

    #[test]
    fn rf_atn_15_tab_marks_take_the_canvas_colours() {
        let sumi = Theme::of(ThemeKind::Sumi);
        let washi = Theme::of(ThemeKind::Washi);
        assert_eq!(
            (sumi.mark_dim, sumi.mark_red, sumi.mark_done, sumi.mark_idle),
            (rgb(0xa19d93), rgb(0xe8333c), rgb(0x6fd39a), rgb(0xcc7a2e))
        );
        assert_eq!(
            (
                washi.mark_dim,
                washi.mark_red,
                washi.mark_done,
                washi.mark_idle
            ),
            (rgb(0x5f5c55), rgb(0xb3121b), rgb(0x22633e), rgb(0xbc6a1e))
        );
        assert_eq!(
            (sumi.word_red, washi.word_red),
            (rgb(0xf0545c), rgb(0xb3121b))
        );
        for t in [sumi, washi] {
            let color = |state| t.tab_mark_color(TabMark::State(state));
            assert_eq!(color(AgentState::NeedsYou), t.mark_red, "{:?}", t.kind);
            assert_eq!(color(AgentState::Error), t.mark_red, "{:?}", t.kind);
            assert_eq!(color(AgentState::Done), t.mark_done, "{:?}", t.kind);
            assert_eq!(color(AgentState::Idle), t.mark_idle, "{:?}", t.kind);
            assert_eq!(color(AgentState::Working), t.ai, "{:?}", t.kind);
            for state in [AgentState::Done, AgentState::Working, AgentState::Idle] {
                assert_eq!(t.tab_word_color(state), t.mark_dim, "{state:?}");
            }
        }
    }

    #[test]
    fn rf_atn_15_tab_marks_stand_out_on_every_row_background() {
        for t in [Theme::of(ThemeKind::Sumi), Theme::of(ThemeKind::Washi)] {
            // The marks are graphics, not text: 3:1 (WCAG 1.4.11).
            let marks = [
                ("red", t.mark_red),
                ("done", t.mark_done),
                ("idle", t.mark_idle),
                (
                    "working",
                    t.tab_mark_color(TabMark::State(AgentState::Working)),
                ),
            ];
            for (name, mark) in marks {
                for bg in [t.panel, t.raised, t.hover, t.term_bg, t.term_raised] {
                    let ratio = contrast(mark, bg);
                    assert!(ratio >= 3.0, "{:?}: {name} is {ratio:.2}:1", t.kind);
                }
            }
        }
    }

    #[test]
    fn rnf_17_working_spinner_uses_ai_token_with_contrast_and_never_red() {
        let working = TabMark::State(AgentState::Working);
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            let color = t.tab_mark_color(working);
            assert_eq!(color, t.ai, "{kind:?}");
            assert_ne!(color, t.mark_red, "{kind:?}: working is never red (RN-01)");
            assert_ne!(color, t.shu, "{kind:?}: working is never red (RN-01)");
            for bg in [t.panel, t.raised, t.hover, t.term_bg, t.term_raised] {
                let ratio = contrast(color, bg);
                assert!(ratio >= 3.0, "{kind:?}: ai on disc bg is {ratio:.2}:1");
            }
        }
    }

    /// CIE L*, the lightness a colour-blind eye still tells apart.
    fn lightness(c: Rgba) -> f32 {
        let y = luminance(c);
        if y > 0.008856 {
            116.0 * y.cbrt() - 16.0
        } else {
            903.3 * y
        }
    }

    #[test]
    fn rnf_17_done_and_idle_dots_differ_in_lightness_not_only_hue() {
        for t in [Theme::of(ThemeKind::Sumi), Theme::of(ThemeKind::Washi)] {
            let gap = (lightness(t.mark_done) - lightness(t.mark_idle)).abs();
            assert!(
                gap >= 15.0,
                "{:?}: green and orange are {gap:.1} L* apart",
                t.kind
            );
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
    fn rf_nav_08_the_focus_ring_is_indigo_and_visible_on_the_bars() {
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            assert_eq!(t.focus_ring(), t.ai, "{kind:?}");
            assert_ne!(t.focus_ring(), t.shu, "{kind:?}");
            for bg in [t.panel, t.raised] {
                let ratio = contrast(t.focus_ring(), bg);
                assert!(ratio >= 3.0, "{kind:?}: the ring is {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn rf_cfg_02_text_sizes_follow_the_configured_ui_size() {
        let t = Theme::of(ThemeKind::Sumi);
        assert_eq!(t.text(12.5), px(12.5), "13 px draws the prototype's sizes");
        let big = t.with_ui_size(26.0);
        assert_eq!(big.text(12.5), px(25.0));
        assert_eq!(big.kind, t.kind);
        assert_eq!(big.shu, t.shu);
    }

    #[test]
    fn rf_ctx_02_the_scroll_thumb_is_neutral_and_stronger_on_hover() {
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            let (rest, strong) = (t.scroll_thumb(false), t.scroll_thumb(true));
            for c in [rest, strong] {
                for red in [t.shu, t.mark_red, t.word_red] {
                    assert_ne!(c, red, "{kind:?}: the thumb is never red (RN-01)");
                }
            }
            assert!(
                contrast(rest, t.term_bg) > 1.2,
                "{kind:?}: the thumb shows at rest"
            );
            assert!(
                contrast(strong, t.term_bg) > contrast(rest, t.term_bg),
                "{kind:?}: hover makes the thumb stronger"
            );
        }
    }

    #[test]
    fn rf_ctx_02_syntax_colors_are_never_red_and_meet_contrast() {
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            for syn in [
                SyntaxKind::Plain,
                SyntaxKind::Keyword,
                SyntaxKind::Type,
                SyntaxKind::Function,
                SyntaxKind::String,
                SyntaxKind::Number,
                SyntaxKind::Comment,
                SyntaxKind::Macro,
            ] {
                let c = t.syntax_color(syn);
                assert_ne!(c, t.shu, "{kind:?}: {syn:?} must not use red (RN-01)");
                assert_ne!(c, t.mark_red, "{kind:?}: {syn:?} must not use mark_red");
                assert_ne!(c, t.word_red, "{kind:?}: {syn:?} must not use word_red");
                let ratio = contrast(c, t.term_bg);
                assert!(
                    ratio >= 4.5,
                    "{kind:?}: {syn:?} contrast on term_bg is {ratio:.2}:1"
                );
            }
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
