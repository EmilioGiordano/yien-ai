//! Which optional title bar pieces to draw at a window width. Flex keeps the
//! window buttons in view; this only trims the pieces that would be clipped.

/// A generous guess at what the window buttons and padding take; flex, not
/// this number, keeps the buttons visible.
const RESERVE: f32 = 150.0;
const RIGHT_PADDING: f32 = 10.0;
const GAP: f32 = 12.0;
const BUTTON: f32 = 30.0;
const LOGO: f32 = 28.0;
const LOGO_TEXT: f32 = 40.0;
const COUNTER: f32 = 170.0;
/// The pill's own width and the share of the bar it may take.
pub(crate) const PILL_WIDTH: f32 = 420.0;
pub(crate) const PILL_SHARE: f32 = 0.4;
pub(crate) const PILL_LEAST: f32 = 60.0;
const PILL_TEXT: f32 = 130.0;
const PILL_KEYS: f32 = 90.0;
const PILL_CHROME: f32 = 40.0;
const PILL_MARGIN: f32 = 1.1;
/// How far the bar's centre sits from the window's: the bar starts after
/// gpui-component's left padding and ends before its three 34 px window
/// buttons. The right column's start margin cancels it.
#[cfg(target_os = "macos")]
pub(crate) const CENTRE_SHIFT: f32 = -80.0;
#[cfg(not(target_os = "macos"))]
pub(crate) const CENTRE_SHIFT: f32 = 3.0 * 34.0 - 12.0;

/// What the title bar draws at a given width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TitleLayout {
    pub pill_keys: bool,
    /// Whether the pill can sit on the window's centre; at narrow widths the
    /// room goes to the pill and the buttons first.
    pub centred: bool,
    pub tab_bar_button: bool,
    pub context_bar_button: bool,
    pub theme_button: bool,
}

impl TitleLayout {
    /// The layout for a window `window` px wide, text scaled by `scale`
    /// (`ui.font.size` over 13). Buttons go theme, context bar, tab bar; the
    /// pill's shortcut goes when the pill has no room for it beside its text.
    pub(crate) fn at(window: f32, scale: f32) -> Self {
        let room = window - RESERVE - RIGHT_PADDING;
        let fixed = LOGO + (LOGO_TEXT + COUNTER) * scale;
        let keys_need = PILL_MARGIN * (PILL_TEXT + PILL_KEYS) * scale + PILL_CHROME;
        let text_need = PILL_MARGIN * PILL_TEXT * scale + PILL_CHROME;
        let mut buttons = 3_usize;
        let pill = loop {
            let gaps = (3 + buttons) as f32 * GAP;
            let rest = room - fixed - buttons as f32 * BUTTON - gaps;
            let pill = rest.min(PILL_WIDTH).min(PILL_SHARE * room);
            if pill >= text_need || buttons == 0 {
                break pill;
            }
            buttons -= 1;
        };
        let gaps = (3 + buttons) as f32 * GAP;
        let slack = room - fixed - buttons as f32 * BUTTON - gaps - pill;
        Self {
            centred: slack >= CENTRE_SHIFT.max(0.0),
            pill_keys: pill >= keys_need,
            tab_bar_button: buttons >= 1,
            context_bar_button: buttons >= 2,
            theme_button: buttons >= 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TitleLayout;

    const BIGGEST: f32 = 20.0 / 13.0;

    #[test]
    fn rnf_19_wide_window_keeps_every_piece() {
        let layout = TitleLayout::at(1600.0, 1.0);
        assert!(layout.pill_keys);
        assert!(layout.theme_button && layout.context_bar_button && layout.tab_bar_button);
    }

    #[test]
    fn rnf_19_narrowing_drops_pieces_in_order_and_never_brings_them_back() {
        for scale in [1.0, 1.25, 1.5, BIGGEST] {
            let mut last = TitleLayout::at(2400.0, scale);
            let mut window = 2400.0;
            while window >= 300.0 {
                let now = TitleLayout::at(window, scale);
                for (before, after) in [
                    (last.pill_keys, now.pill_keys),
                    (last.theme_button, now.theme_button),
                    (last.context_bar_button, now.context_bar_button),
                    (last.tab_bar_button, now.tab_bar_button),
                ] {
                    assert!(
                        before || !after,
                        "a piece came back at {window}, scale {scale}"
                    );
                }
                assert!(now.tab_bar_button || !now.context_bar_button);
                assert!(now.context_bar_button || !now.theme_button);
                last = now;
                window -= 5.0;
            }
        }
    }

    #[test]
    fn rnf_19_the_minimum_window_keeps_the_toggles_at_the_default_size() {
        let layout = TitleLayout::at(960.0, 1.0);
        assert!(layout.theme_button && layout.context_bar_button && layout.tab_bar_button);
    }

    #[test]
    fn rf_nav_01_the_shortcut_hides_before_the_pill_text_can_be_cut() {
        assert!(!TitleLayout::at(960.0, BIGGEST).pill_keys);
        assert!(TitleLayout::at(960.0, 1.0).pill_keys);
    }

    #[test]
    fn rf_nav_01_pill_is_centred_when_wide_and_centring_gives_way_once_for_good() {
        assert!(TitleLayout::at(1600.0, 1.0).centred);
        assert!(!TitleLayout::at(960.0, BIGGEST).centred);
        for scale in [1.0, 1.25, 1.5, BIGGEST] {
            let mut window = 2400.0;
            let mut gave_way = false;
            while window >= 300.0 {
                let now = TitleLayout::at(window, scale);
                assert!(!gave_way || !now.centred, "centring came back at {window}");
                if !now.centred {
                    gave_way = true;
                }
                window -= 5.0;
            }
        }
    }
}
