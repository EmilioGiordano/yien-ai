//! The prototype's three families, each falling back to what Windows ships.
//! Fonts are not bundled yet, so the choice depends on what is installed.

use gpui_kit::SharedString;

const UI: &[&str] = &["Zen Kaku Gothic New", "Yu Gothic UI", "Segoe UI"];
const MONO: &[&str] = &["M PLUS 1 Code", "Cascadia Mono", "Consolas"];
const DISPLAY: &[&str] = &["Shippori Mincho B1", "Yu Mincho", "Georgia"];

/// `None` for a role means GPUI's default family.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Fonts {
    pub(crate) ui: Option<SharedString>,
    pub(crate) mono: Option<SharedString>,
    pub(crate) display: Option<SharedString>,
}

impl Fonts {
    pub(crate) fn resolve(installed: &[String]) -> Self {
        let pick = |candidates| pick_font(installed, candidates).map(SharedString::new_static);
        Self {
            ui: pick(UI),
            mono: pick(MONO),
            display: pick(DISPLAY),
        }
    }
}

/// The first candidate that is installed, in the candidates' order.
fn pick_font(installed: &[String], candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .find(|c| installed.iter().any(|i| i.eq_ignore_ascii_case(c)))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn the_preferred_family_wins_when_installed() {
        let fonts = Fonts::resolve(&installed(&[
            "Consolas",
            "M PLUS 1 Code",
            "Yu Gothic UI",
            "Zen Kaku Gothic New",
        ]));
        assert_eq!(fonts.ui.as_deref(), Some("Zen Kaku Gothic New"));
        assert_eq!(fonts.mono.as_deref(), Some("M PLUS 1 Code"));
    }

    #[test]
    fn a_missing_family_falls_back_in_order() {
        let fonts = Fonts::resolve(&installed(&["consolas", "Cascadia Mono", "Yu Mincho"]));
        assert_eq!(fonts.mono.as_deref(), Some("Cascadia Mono"));
        assert_eq!(fonts.display.as_deref(), Some("Yu Mincho"));
        assert_eq!(fonts.ui, None);
    }

    #[test]
    fn nothing_installed_leaves_the_default_family() {
        assert_eq!(Fonts::resolve(&[]), Fonts::default());
    }
}
