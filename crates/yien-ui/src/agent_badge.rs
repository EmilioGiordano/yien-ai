//! The round badge of the program a tab or pane runs. An agent CLI's is its
//! publisher's mark in its own colours: never tinted, the same in both
//! themes; the marks and their notice are in `assets/third-party/agents/`.
//! A shell's is a disc in its colour with a short glyph, drawn by YIEN.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, DevicePixels, Div, FontWeight, Global, ImageCacheError, ParentElement, RenderImage,
    SharedString, Styled, SvgRenderer, SvgSize, Window, div, img, px, rgb, size,
};
use yien_proto::ShellKind;

/// The badge's side, in px at any UI font size.
pub(crate) const SIDE: f32 = 18.0;
/// The side of the disc a corner mark sits on.
pub(crate) const CORNER: f32 = 10.0;
/// How far the corner disc reaches past the badge's right and bottom edges.
pub(crate) const CORNER_OVERHANG: f32 = 2.0;

/// What a tab row or pane head badge shows: an agent's official mark or a
/// shell's mark that YIEN draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Badge {
    Agent(AgentBadge),
    Shell(ShellBadge),
}

/// A shell's badge: a disc in the shell's colour with a short glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ShellBadge {
    /// Windows PowerShell and PowerShell 7.
    PowerShell,
    Cmd,
    GitBash,
    /// A shell not recognised, such as `$SHELL` off Windows.
    Other,
}

/// How a shell badge is drawn: its disc, an optional inset ring, the glyph,
/// its colour, weight and size in px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ShellLook {
    pub(crate) disc: u32,
    pub(crate) ring: Option<u32>,
    pub(crate) glyph: &'static str,
    pub(crate) ink: u32,
    pub(crate) bold: bool,
    pub(crate) size: f32,
}

impl ShellBadge {
    /// The badge of a shell started by its own profile.
    pub(crate) fn of_kind(kind: ShellKind) -> Self {
        match kind {
            ShellKind::PowerShell | ShellKind::Pwsh => Self::PowerShell,
            ShellKind::Cmd => Self::Cmd,
            ShellKind::GitBash => Self::GitBash,
        }
    }

    /// The badge of the default shell by its program's name, as the core
    /// names the pane. `bash` is Git Bash only on Windows, the only bash
    /// YIEN starts there.
    pub(crate) fn of_program(name: &str, windows: bool) -> Self {
        match name {
            "pwsh" | "powershell" => Self::PowerShell,
            "cmd" => Self::Cmd,
            "bash" if windows => Self::GitBash,
            _ => Self::Other,
        }
    }

    pub(crate) fn look(self) -> ShellLook {
        let look = |disc, ring, glyph, ink, bold, size| ShellLook {
            disc,
            ring,
            glyph,
            ink,
            bold,
            size,
        };
        match self {
            Self::PowerShell => look(0x2671BE, None, ">_", 0xFFFFFF, true, 9.0),
            Self::Cmd => look(0x0C0C0C, Some(0x3A3631), "C:", 0xE6E6E6, false, 9.0),
            Self::GitBash => look(0xF05033, None, "$", 0xFFFFFF, true, 11.0),
            Self::Other => look(0x2B2A28, None, ">_", 0xE6E6E6, true, 9.0),
        }
    }
}

/// An agent CLI with an official mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AgentBadge {
    Claude,
    Codex,
    OpenCode,
}

#[cfg(test)]
impl AgentBadge {
    pub(crate) const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::OpenCode];

    /// The bundled file this badge draws.
    pub(crate) fn file_name(self) -> &'static str {
        self.art().0
    }
}

impl AgentBadge {
    /// The bundled mark: its file name in the notice and its bytes.
    fn art(self) -> (&'static str, &'static [u8]) {
        match self {
            Self::Claude => (
                "ClaudeIcon-Rounded.svg",
                include_bytes!("../assets/third-party/agents/ClaudeIcon-Rounded.svg"),
            ),
            Self::Codex => (
                "OAI_OpenAI-Blossom_White.svg",
                include_bytes!("../assets/third-party/agents/OAI_OpenAI-Blossom_White.svg"),
            ),
            Self::OpenCode => (
                "opencode-logo-dark-square.svg",
                include_bytes!("../assets/third-party/agents/opencode-logo-dark-square.svg"),
            ),
        }
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
            Self::Codex => 22.0,
            Self::OpenCode => 11.0,
        }
    }

    /// Whether the file itself is cut to the disc: the Claude icon is a
    /// rounded square edge to edge.
    fn clipped(self) -> bool {
        self == Self::Claude
    }
}

/// The badge, [`SIDE`] px and fixed, with `corner` on a [`CORNER`] px disc
/// over its bottom-right edge. The caller fills the corner (its ground colour
/// and its mark); this only places and rounds it. A shell's glyph is set in
/// `mono`. Decorative: no id or label, as the row and head name the program
/// and state.
pub(crate) fn badge(badge: Badge, corner: Option<Div>, mono: Option<SharedString>) -> Div {
    let disc = div().flex_none().relative().size(px(SIDE)).rounded_full();
    let disc = match badge {
        Badge::Agent(agent) => {
            let art = agent.art_side();
            let offset = (SIDE - art) / 2.0;
            disc.when_some(agent.ground(), |d, ground| d.bg(rgb(ground)))
                .child(
                    img(move |window: &mut Window, cx: &mut App| {
                        Some(raster(agent, art, window, cx))
                    })
                    .absolute()
                    .left(px(offset))
                    .top(px(offset))
                    .size(px(art))
                    .when(agent.clipped(), |i| i.rounded_full()),
                )
        }
        Badge::Shell(shell) => {
            let look = shell.look();
            disc.bg(rgb(look.disc))
                .when_some(look.ring, |d, ring| d.border_1().border_color(rgb(ring)))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(look.size))
                .line_height(px(SIDE))
                .text_color(rgb(look.ink))
                .when(look.bold, |d| d.font_weight(FontWeight::BOLD))
                .when_some(mono, |d, family| d.font_family(family))
                .child(look.glyph)
        }
    };
    disc.when_some(corner, |d, corner| {
        d.child(
            corner
                .absolute()
                .right(px(-CORNER_OVERHANG))
                .bottom(px(-CORNER_OVERHANG))
                .size(px(CORNER))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden(),
        )
    })
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
            .as_chunks::<4>()
            .0
            .iter()
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
        // The Blossom fills about 49% of its file, so it shows near 11 px.
        let blossom = AgentBadge::Codex.art_side() * 348.5 / 716.0;
        assert!((10.0..=12.0).contains(&blossom), "{blossom}");
    }

    #[test]
    fn the_corner_disc_stays_small_and_close_to_the_badge() {
        // It covers the badge's corner, not its middle, and reaches past the
        // badge less than the 8 px gap to the next piece and the 5 px a row or
        // head pads above and below, so nothing clips or overlaps it.
        let start = SIDE + CORNER_OVERHANG - CORNER;
        assert!(start > SIDE / 2.0, "{start}");
        const { assert!(CORNER_OVERHANG < 5.0) };
        const { assert!(CORNER >= 9.0, "room for a mark glyph") };
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

    const SHELLS: [ShellBadge; 4] = [
        ShellBadge::PowerShell,
        ShellBadge::Cmd,
        ShellBadge::GitBash,
        ShellBadge::Other,
    ];

    #[test]
    fn rf_pan_12_each_shell_kind_and_program_has_its_badge() {
        let kinds = [
            (ShellKind::PowerShell, ShellBadge::PowerShell),
            (ShellKind::Pwsh, ShellBadge::PowerShell),
            (ShellKind::Cmd, ShellBadge::Cmd),
            (ShellKind::GitBash, ShellBadge::GitBash),
        ];
        for (kind, badge) in kinds {
            assert_eq!(ShellBadge::of_kind(kind), badge, "{kind:?}");
        }
        for windows in [true, false] {
            assert_eq!(
                ShellBadge::of_program("pwsh", windows),
                ShellBadge::PowerShell
            );
            assert_eq!(
                ShellBadge::of_program("powershell", windows),
                ShellBadge::PowerShell
            );
            assert_eq!(ShellBadge::of_program("cmd", windows), ShellBadge::Cmd);
            assert_eq!(ShellBadge::of_program("zsh", windows), ShellBadge::Other);
            assert_eq!(
                ShellBadge::of_program("my-pane", windows),
                ShellBadge::Other
            );
        }
        assert_eq!(ShellBadge::of_program("bash", true), ShellBadge::GitBash);
        assert_eq!(ShellBadge::of_program("bash", false), ShellBadge::Other);
    }

    #[test]
    fn each_shell_badge_has_the_chosen_look() {
        let got: Vec<_> = SHELLS
            .iter()
            .map(|s| {
                let l = s.look();
                (l.disc, l.ring, l.glyph, l.ink)
            })
            .collect();
        assert_eq!(
            got,
            [
                (0x2671BE, None, ">_", 0xFFFFFF),
                (0x0C0C0C, Some(0x3A3631), "C:", 0xE6E6E6),
                (0xF05033, None, "$", 0xFFFFFF),
                (0x2B2A28, None, ">_", 0xE6E6E6),
            ]
        );
    }

    /// WCAG relative luminance of `0xRRGGBB`.
    fn luminance(rgb: u32) -> f32 {
        let [_, r, g, b] = rgb.to_be_bytes();
        let lin = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn rnf_17_a_shell_glyph_stands_out_from_its_disc() {
        for shell in SHELLS {
            let look = shell.look();
            let ratio = contrast(look.ink, look.disc);
            assert!(ratio >= 3.0, "{shell:?}: {ratio:.2}:1");
        }
    }

    #[test]
    fn a_shell_glyph_fits_inside_its_disc() {
        // A mono advance is about 0.6 em; the glyph's box, with its line,
        // stays inside the circle's inscribed square.
        let inscribed = SIDE / std::f32::consts::SQRT_2;
        for shell in SHELLS {
            let look = shell.look();
            let width = look.glyph.chars().count() as f32 * 0.6 * look.size;
            assert!(width <= inscribed, "{shell:?} is {width} px wide");
            assert!(look.size <= inscribed, "{shell:?} is {} px tall", look.size);
            assert!(look.size >= 9.0, "{shell:?} stays legible");
        }
    }
}
