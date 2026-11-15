//! The round badge of the agent CLI a tab or pane runs, in the publisher's
//! own colours: never tinted, the same in both themes. The marks and their
//! notice are in `assets/third-party/agents/`.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, DevicePixels, Div, Global, ImageCacheError, ParentElement, RenderImage, Styled,
    SvgRenderer, SvgSize, Window, div, img, px, rgb, size,
};

/// The badge's side, in px at any UI font size, like the tab row's mark slot.
pub(crate) const SIDE: f32 = 16.0;

/// An agent CLI with an official mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AgentBadge {
    Claude,
    Codex,
    OpenCode,
}

/// The bundled marks, each with its file name in the notice.
const ART: [(AgentBadge, &str, &[u8]); 3] = [
    (
        AgentBadge::Claude,
        "ClaudeIcon-Rounded.svg",
        include_bytes!("../assets/third-party/agents/ClaudeIcon-Rounded.svg"),
    ),
    (
        AgentBadge::Codex,
        "OAI_OpenAI-Blossom_White.svg",
        include_bytes!("../assets/third-party/agents/OAI_OpenAI-Blossom_White.svg"),
    ),
    (
        AgentBadge::OpenCode,
        "opencode-logo-dark-square.svg",
        include_bytes!("../assets/third-party/agents/opencode-logo-dark-square.svg"),
    ),
];

#[cfg(test)]
impl AgentBadge {
    pub(crate) const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::OpenCode];

    /// The bundled file this badge draws.
    pub(crate) fn file_name(self) -> &'static str {
        self.art().0
    }
}

impl AgentBadge {
    fn art(self) -> (&'static str, &'static [u8]) {
        let (_, name, bytes) = ART
            .into_iter()
            .find(|(badge, _, _)| *badge == self)
            .unwrap_or(ART[0]);
        (name, bytes)
    }

    /// The disc behind the mark, for a mark drawn without its own ground:
    /// the white Blossom on black, OpenCode's dark mark on the `#1F1F1F` of
    /// its light variant. Claude's icon brings its clay.
    pub(crate) fn ground(self) -> Option<u32> {
        match self {
            Self::Claude => None,
            Self::Codex => Some(0x000000),
            Self::OpenCode => Some(0x1F1F1F),
        }
    }

    /// The side of the square the whole file is drawn in, centred on the
    /// badge. The Blossom's file is twice its mark, so it overflows the badge
    /// with transparent margin; OpenCode's 4:5 mark shrinks to fit the disc.
    pub(crate) fn art_side(self) -> f32 {
        match self {
            Self::Claude => SIDE,
            Self::Codex => 20.0,
            Self::OpenCode => 10.0,
        }
    }

    /// Whether the file itself is cut to the disc: the Claude icon is a
    /// rounded square edge to edge.
    fn clipped(self) -> bool {
        self == Self::Claude
    }
}

/// The badge slot: empty for `None`, keeping its width so names align.
/// Decorative, so it carries no id or label; the row and head already name
/// the program.
pub(crate) fn badge_slot(badge: Option<AgentBadge>) -> Div {
    let slot = div().flex_none().relative().size(px(SIDE));
    let Some(badge) = badge else {
        return slot;
    };
    let art = badge.art_side();
    let offset = (SIDE - art) / 2.0;
    slot.rounded_full()
        .when_some(badge.ground(), |d, ground| d.bg(rgb(ground)))
        .child(
            img(move |window: &mut Window, cx: &mut App| Some(raster(badge, art, window, cx)))
                .absolute()
                .left(px(offset))
                .top(px(offset))
                .size(px(art))
                .when(badge.clipped(), |i| i.rounded_full()),
        )
}

/// Rasters at the window's scale, made once per badge and size.
#[derive(Default)]
struct Rasters(HashMap<(AgentBadge, i32), Arc<RenderImage>>);

impl Global for Rasters {}

/// The badge's mark at `art` px, rastered at the device size so it is drawn
/// 1:1: GPUI's own `img` path rasters an SVG at its file size, which for the
/// 716 px Blossom would be sampled down twentyfold.
fn raster(
    badge: AgentBadge,
    art: f32,
    window: &mut Window,
    cx: &mut App,
) -> Result<Arc<RenderImage>, ImageCacheError> {
    let side = device_side(art, window.scale_factor());
    if let Some(hit) = cx
        .try_global::<Rasters>()
        .and_then(|r| r.0.get(&(badge, side)))
    {
        return Ok(Arc::clone(hit));
    }
    let image = render(&cx.svg_renderer(), badge, side)?;
    cx.default_global::<Rasters>()
        .0
        .insert((badge, side), Arc::clone(&image));
    Ok(image)
}

/// `art` logical px in device px, at least one.
fn device_side(art: f32, scale: f32) -> i32 {
    ((art * scale).round() as i32).max(1)
}

/// The badge's file rastered to a `side` px square, in its own colours.
fn render(
    renderer: &SvgRenderer,
    badge: AgentBadge,
    side: i32,
) -> Result<Arc<RenderImage>, ImageCacheError> {
    let svg = renderer.parse_svg(badge.art().1)?;
    let square = size(DevicePixels(side), DevicePixels(side));
    Ok(renderer.render_parsed(&svg, SvgSize::ExactSize(square))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTICE: &str = include_str!("../assets/third-party/agents/NOTICE.md");

    fn renderer() -> SvgRenderer {
        SvgRenderer::new(Arc::new(crate::assets::YienAssets))
    }

    /// The opaque pixels of `badge` rastered at `side`, as `0xRRGGBB`.
    fn opaque(badge: AgentBadge, side: i32) -> Vec<u32> {
        let image = render(&renderer(), badge, side).unwrap();
        let got = image.size(0);
        assert_eq!(
            (got.width, got.height),
            (DevicePixels(side), DevicePixels(side))
        );
        image
            .as_bytes(0)
            .unwrap()
            .chunks_exact(4)
            .filter(|bgra| bgra[3] == 255)
            .map(|bgra| u32::from_be_bytes([0, bgra[2], bgra[1], bgra[0]]))
            .collect()
    }

    #[test]
    fn each_mark_rasters_in_its_official_colours() {
        // Claude: ivory spark on the clay gradient, #DC6038 to #D97757 under
        // a 35% #D97757 wash.
        let claude = opaque(AgentBadge::Claude, 64);
        assert!(claude.contains(&0xFAF9F5), "the ivory spark");
        let clay = |c: &u32| {
            let [_, r, g, b] = c.to_be_bytes();
            (0xD9..=0xDC).contains(&r) && (0x60..=0x80).contains(&g) && (0x38..=0x5A).contains(&b)
        };
        assert!(claude.iter().filter(|c| clay(c)).count() > 64 * 64 / 4);
        // The Blossom is white and nothing else.
        let codex = opaque(AgentBadge::Codex, 64);
        assert!(!codex.is_empty());
        assert!(codex.iter().all(|c| *c == 0xFFFFFF), "not recoloured");
        // OpenCode's dark mark keeps both its greys through mask and clip.
        let opencode = opaque(AgentBadge::OpenCode, 60);
        assert!(opencode.contains(&0xEEEEEE) && opencode.contains(&0x4B4B4B));
    }

    #[test]
    fn a_mark_is_rastered_at_the_device_size() {
        assert_eq!(device_side(16.0, 1.0), 16);
        assert_eq!(device_side(16.0, 1.25), 20);
        assert_eq!(device_side(10.0, 1.25), 13);
        assert_eq!(device_side(20.0, 1.5), 30);
        assert_eq!(device_side(0.1, 1.0), 1);
        for badge in AgentBadge::ALL {
            let image = render(&renderer(), badge, 20).unwrap();
            assert_eq!(image.size(0).width, DevicePixels(20), "{badge:?}");
        }
    }

    #[test]
    fn each_mark_sits_on_a_disc_of_its_own() {
        for badge in AgentBadge::ALL {
            let art = badge.art_side();
            // Drawn edge to edge it is cut to the disc; else a ground fills it.
            assert!(badge.clipped() || badge.ground().is_some(), "{badge:?}");
            if badge.clipped() {
                assert_eq!(art, SIDE);
            }
        }
        // OpenCode's 4:5 mark has its corners inside the disc.
        let art = AgentBadge::OpenCode.art_side();
        let corner = ((0.4 * art).powi(2) + (0.5 * art).powi(2)).sqrt();
        assert!(corner <= SIDE / 2.0);
        // The Blossom fills about 49% of its file, so it shows near 10 px.
        let blossom = AgentBadge::Codex.art_side() * 348.5 / 716.0;
        assert!((9.0..=11.0).contains(&blossom), "{blossom}");
    }

    #[test]
    fn every_bundled_file_is_in_the_notice_and_drawn() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("third-party")
            .join("agents");
        let mut bundled: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n != "NOTICE.md")
            .collect();
        bundled.sort();
        let mut drawn: Vec<String> = AgentBadge::ALL
            .iter()
            .map(|b| b.file_name().to_owned())
            .collect();
        drawn.sort();
        assert_eq!(
            bundled, drawn,
            "a bundled file nothing draws, or the reverse"
        );
        for name in &bundled {
            assert!(
                NOTICE.contains(&format!("`{name}`")),
                "{name} not in NOTICE.md"
            );
        }
    }

    #[test]
    fn the_notice_carries_the_trademark_lines_and_the_opencode_licence() {
        for line in [
            "Claude and the Claude logo are trademarks of Anthropic PBC.",
            "OpenAI and the OpenAI logo are trademarks of OpenAI.",
            "OpenCode and the OpenCode logo are trademarks of Anomaly.",
            "MIT License",
            "Copyright (c) 2025 opencode",
            "not covered by YIEN's GPL licence",
            "unmodified",
        ] {
            assert!(NOTICE.contains(line), "{line}");
        }
    }

    #[test]
    fn no_bundled_mark_carries_style_script_or_raster() {
        for badge in AgentBadge::ALL {
            let text = std::str::from_utf8(badge.art().1).unwrap();
            for banned in ["<style", "<script", "<image", "data:"] {
                assert!(!text.contains(banned), "{badge:?} has {banned}");
            }
        }
    }
}
