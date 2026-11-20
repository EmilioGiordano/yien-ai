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

/// The working spinner's 8 frames, one per [`SPINNER_FRAME`].
pub(crate) const SPINNER: [&str; 8] = [
    "yien/spinner-0.svg",
    "yien/spinner-1.svg",
    "yien/spinner-2.svg",
    "yien/spinner-3.svg",
    "yien/spinner-4.svg",
    "yien/spinner-5.svg",
    "yien/spinner-6.svg",
    "yien/spinner-7.svg",
];

const OWN: [(&str, &[u8]); 21] = [
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
    (SPINNER[0], include_bytes!("../assets/spinner-0.svg")),
    (SPINNER[1], include_bytes!("../assets/spinner-1.svg")),
    (SPINNER[2], include_bytes!("../assets/spinner-2.svg")),
    (SPINNER[3], include_bytes!("../assets/spinner-3.svg")),
    (SPINNER[4], include_bytes!("../assets/spinner-4.svg")),
    (SPINNER[5], include_bytes!("../assets/spinner-5.svg")),
    (SPINNER[6], include_bytes!("../assets/spinner-6.svg")),
    (SPINNER[7], include_bytes!("../assets/spinner-7.svg")),
];

pub(crate) const NEEDS_YOU_MARK: &str = "■";
pub(crate) const ERROR_MARK: &str = "!";
pub(crate) const DONE_MARK: &str = "●";
pub(crate) const IDLE_MARK: &str = "●";

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

/// A visual representation of a state mark: a text glyph in the mono font,
/// or an SVG asset path for the circular working spinner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MarkVisual {
    Glyph(MarkGlyph),
    Spinner(&'static str),
}

/// A tab row's mark visual: static glyphs for needs you, error, done and idle;
/// the circular spinner asset for working; `None` for a tab of shells only.
pub(crate) fn tab_mark_visual(mark: TabMark, frame: usize) -> Option<MarkVisual> {
    let glyph = |text, size, bold| Some(MarkVisual::Glyph(MarkGlyph { text, size, bold }));
    match mark {
        TabMark::State(AgentState::NeedsYou) => glyph(NEEDS_YOU_MARK, 13.5, false),
        TabMark::State(AgentState::Error) => glyph(ERROR_MARK, 13.5, true),
        TabMark::State(AgentState::Done) => glyph(DONE_MARK, 11.0, false),
        TabMark::State(AgentState::Working) => {
            Some(MarkVisual::Spinner(SPINNER[frame % SPINNER.len()]))
        }
        TabMark::State(AgentState::Idle) => glyph(IDLE_MARK, 11.0, false),
        TabMark::Shell => None,
    }
}

/// A state mark in an agent badge's corner: glyphs are scaled from the 14 px
/// slot to the 10 px corner disc; the circular spinner uses its 10 px asset.
pub(crate) fn corner_mark_visual(mark: TabMark, frame: usize) -> Option<MarkVisual> {
    match tab_mark_visual(mark, frame)? {
        MarkVisual::Glyph(glyph) => Some(MarkVisual::Glyph(MarkGlyph {
            size: glyph.size * crate::agent_badge::CORNER / 14.0,
            ..glyph
        })),
        MarkVisual::Spinner(path) => Some(MarkVisual::Spinner(path)),
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
    fn every_own_asset_is_an_embedded_svg() {
        for (path, _) in OWN {
            let bytes = YienAssets.load(path).unwrap().unwrap();
            assert!(bytes.starts_with(b"<svg"), "{path}");
        }
    }

    #[test]
    fn rn_01_needs_you_and_error_share_the_red_and_differ_by_shape() {
        use crate::theme::{Theme, ThemeKind};
        let needs = TabMark::State(AgentState::NeedsYou);
        let error = TabMark::State(AgentState::Error);
        assert_eq!(tab_mark_text(needs, 0), NEEDS_YOU_MARK);
        assert_eq!(tab_mark_text(error, 0), ERROR_MARK);
        assert_ne!(NEEDS_YOU_MARK, ERROR_MARK);
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            assert_eq!(t.tab_mark_color(needs), t.tab_mark_color(error), "{kind:?}");
        }
    }

    fn expect_glyph(visual: Option<MarkVisual>) -> MarkGlyph {
        match visual {
            Some(MarkVisual::Glyph(glyph)) => glyph,
            other => panic!("expected glyph, got {other:?}"),
        }
    }

    fn tab_mark_text(mark: TabMark, frame: usize) -> &'static str {
        expect_glyph(tab_mark_visual(mark, frame)).text
    }

    #[test]
    fn tab_marks_are_the_canvas_glyphs_at_their_sizes() {
        let want = [
            (AgentState::NeedsYou, "■", 13.5, false),
            (AgentState::Error, "!", 13.5, true),
            (AgentState::Done, "●", 11.0, false),
            (AgentState::Idle, "●", 11.0, false),
        ];
        for (state, text, size, bold) in want {
            let got = expect_glyph(tab_mark_visual(TabMark::State(state), 0));
            assert_eq!(
                (got.text, got.size, got.bold),
                (text, size, bold),
                "{state:?}"
            );
            assert!(got.size <= 14.0, "{state:?} fits its 14 px slot");
        }
        assert_eq!(
            tab_mark_visual(TabMark::State(AgentState::Working), 0),
            Some(MarkVisual::Spinner(SPINNER[0]))
        );
    }

    #[test]
    fn rn_01_a_corner_mark_keeps_the_glyph_and_fits_its_disc() {
        use crate::agent_badge::CORNER;
        for state in [
            AgentState::NeedsYou,
            AgentState::Error,
            AgentState::Done,
            AgentState::Idle,
        ] {
            let mark = TabMark::State(state);
            let full = expect_glyph(tab_mark_visual(mark, 3));
            let corner = expect_glyph(corner_mark_visual(mark, 3));
            assert_eq!((corner.text, corner.bold), (full.text, full.bold));
            assert!(corner.size < CORNER, "{state:?} fits the {CORNER} px disc");
            assert!(corner.size >= 7.5, "{state:?} stays legible");
        }
        assert_eq!(
            corner_mark_visual(TabMark::State(AgentState::Working), 3),
            Some(MarkVisual::Spinner(SPINNER[3]))
        );
        assert_eq!(corner_mark_visual(TabMark::Shell, 0), None);
    }

    #[test]
    fn a_tab_of_shells_only_has_no_mark() {
        assert_eq!(tab_mark_visual(TabMark::Shell, 0), None);
        assert_eq!(tab_mark_visual(TabMark::Shell, 7), None);
        assert_eq!(corner_mark_visual(TabMark::Shell, 0), None);
        assert_eq!(corner_mark_visual(TabMark::Shell, 7), None);
    }

    #[test]
    fn every_tab_mark_differs_by_shape_or_colour_in_both_themes() {
        use crate::theme::{Theme, ThemeKind};
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            let look = |s| {
                let mark = TabMark::State(s);
                (tab_mark_visual(mark, 0), t.tab_mark_color(mark))
            };
            for (i, a) in AgentState::BY_URGENCY.into_iter().enumerate() {
                for b in AgentState::BY_URGENCY.into_iter().skip(i + 1) {
                    assert_ne!(look(a), look(b), "{kind:?}: {a:?} and {b:?}");
                }
            }
        }
    }

    /// Parses `<circle cx="..." cy="..." r="..."/>` elements from an embedded SVG.
    fn parse_svg_circles(svg: &str) -> Vec<(f32, f32, f32)> {
        let attr = |tag: &str, name: &str| -> f32 {
            let needle = format!(r#"{name}=""#);
            let start = tag
                .find(&needle)
                .unwrap_or_else(|| panic!("missing {name} in {tag}"))
                + needle.len();
            let end = tag[start..].find('"').unwrap() + start;
            tag[start..end].parse::<f32>().unwrap()
        };
        svg.split("<circle ")
            .skip(1)
            .map(|rest| {
                let tag = rest.split("/>").next().unwrap();
                (attr(tag, "cx"), attr(tag, "cy"), attr(tag, "r"))
            })
            .collect()
    }

    #[test]
    fn rf_atn_15_spinner_frame_geometry_stays_strictly_inside_corner_disc() {
        use crate::agent_badge::CORNER;
        use gpui_kit::{DevicePixels, SvgRenderer, SvgSize, size};
        use std::sync::Arc;

        let disc_size = CORNER;
        let disc_radius = disc_size / 2.0;
        assert_eq!(disc_size, 10.0);
        assert_eq!(SPINNER.len(), 8);

        let renderer = SvgRenderer::new(Arc::new(YienAssets));
        for (frame, &path) in SPINNER.iter().enumerate() {
            let bytes = YienAssets.load(path).unwrap().unwrap();
            let svg = std::str::from_utf8(&bytes).unwrap();
            assert!(
                svg.contains(r#"viewBox="0 0 10 10""#),
                "{path} must match the 10 px corner disc"
            );
            assert!(
                svg.contains(r##"fill="#000""##),
                "{path} must be monochrome"
            );

            let circles = parse_svg_circles(svg);
            assert_eq!(circles.len(), 3, "{path} must have a 3-dot snake");
            for (offset, &(cx, cy, r)) in circles.iter().enumerate() {
                let slot = (frame + offset) % SPINNER.len();
                let angle = (-90.0 + slot as f32 * 45.0).to_radians();
                let want_cx = disc_radius + 3.2 * angle.cos();
                let want_cy = disc_radius + 3.2 * angle.sin();
                assert!(
                    (cx - want_cx).abs() < 0.01 && (cy - want_cy).abs() < 0.01,
                    "{path} dot {offset} at ({cx}, {cy}) != expected slot {slot} ({want_cx:.2}, {want_cy:.2})"
                );
                assert!(r > 0.0, "{path} dot {offset} has positive radius");
                let dist = ((cx - disc_radius).powi(2) + (cy - disc_radius).powi(2)).sqrt();
                assert!(
                    dist + r < disc_radius,
                    "{path} dot {offset} spills outside corner disc: {dist} + {r} >= {disc_radius}"
                );
                assert!(cx - r >= 0.0 && cx + r <= disc_size, "{path}");
                assert!(cy - r >= 0.0 && cy + r <= disc_size, "{path}");
            }

            let parsed = renderer.parse_svg(&bytes).unwrap();
            for side in [10, 13, 15, 20] {
                let image = renderer
                    .render_parsed(
                        &parsed,
                        SvgSize::ExactSize(size(DevicePixels(side), DevicePixels(side))),
                    )
                    .unwrap();
                let pixels = image.as_bytes(0).unwrap();
                let half = side as f32 / 2.0;
                let mut painted = 0usize;
                for y in 0..side {
                    for x in 0..side {
                        let i = ((y * side + x) * 4) as usize;
                        if pixels[i + 3] > 0 {
                            painted += 1;
                            let dx = x as f32 + 0.5 - half;
                            let dy = y as f32 + 0.5 - half;
                            assert!(
                                dx * dx + dy * dy < half * half,
                                "{path} at {side}px paints outside disc at ({x}, {y})"
                            );
                        }
                    }
                }
                assert!(painted > 0, "{path} at {side}px painted nothing");
            }
        }
    }

    #[test]
    fn rf_atn_15_the_spinner_runs_its_eight_frames_and_wraps() {
        assert_eq!(SPINNER.len(), 8);
        assert_eq!(SPINNER_FRAME, Duration::from_millis(80));
        let working = TabMark::State(AgentState::Working);
        let mut frame = 0;
        let mut seen = Vec::new();
        for _ in 0..SPINNER.len() {
            let visual = tab_mark_visual(working, frame).unwrap();
            let MarkVisual::Spinner(path) = visual else {
                panic!("working must be a spinner");
            };
            seen.push(path);
            frame = next_frame(frame);
        }
        assert_eq!(seen, SPINNER);
        assert_eq!(frame, 0, "the last frame wraps to the first");
        assert_eq!(
            tab_mark_visual(working, SPINNER.len() + 3),
            Some(MarkVisual::Spinner(SPINNER[3]))
        );
        let idle = TabMark::State(AgentState::Idle);
        assert_eq!(tab_mark_visual(idle, 4), tab_mark_visual(idle, 0));
    }

    #[test]
    fn rf_atn_15_every_former_spinner_caller_uses_the_new_circular_spinner() {
        let working = TabMark::State(AgentState::Working);
        for (frame, &path) in SPINNER.iter().enumerate() {
            assert_eq!(
                tab_mark_visual(working, frame),
                Some(MarkVisual::Spinner(path))
            );
            assert_eq!(
                corner_mark_visual(working, frame),
                Some(MarkVisual::Spinner(path))
            );
            for state in [
                AgentState::NeedsYou,
                AgentState::Error,
                AgentState::Done,
                AgentState::Idle,
            ] {
                let visual = tab_mark_visual(TabMark::State(state), frame).unwrap();
                assert!(matches!(visual, MarkVisual::Glyph(_)), "{state:?}");
            }
            assert_eq!(tab_mark_visual(TabMark::Shell, frame), None);
            assert_eq!(corner_mark_visual(TabMark::Shell, frame), None);
        }
    }

    #[test]
    fn own_assets_are_listed_and_kit_icons_still_load() {
        let listed = YienAssets.list("yien/").unwrap();
        assert_eq!(listed.len(), OWN.len());
        assert!(YienAssets.load("icons/window-close.svg").unwrap().is_some());
    }
}
