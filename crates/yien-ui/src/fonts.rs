//! The prototype's three families, each falling back to what Windows ships.
//! Fonts are not bundled yet, so the choice depends on what is installed.

use gpui_kit::SharedString;

// Japanese UI faces draw U+005C as a yen sign: paths and branches use MONO.
const UI: &[&str] = &["Zen Kaku Gothic New", "Yu Gothic UI", "Segoe UI"];
const MONO: &[&str] = &["M PLUS 1 Code", "Cascadia Mono", "Consolas"];
const DISPLAY: &[&str] = &["Shippori Mincho B1", "Yu Mincho", "Georgia"];

/// `None` for a role means GPUI's default family.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Fonts {
    pub(crate) ui: Option<SharedString>,
    pub(crate) mono: Option<SharedString>,
    pub(crate) display: Option<SharedString>,
    /// The terminals' family: [`Fonts::mono`] unless the settings name one.
    pub(crate) terminal: Option<SharedString>,
}

impl Fonts {
    pub(crate) fn resolve(installed: &[String]) -> Self {
        let pick = |candidates| pick_font(installed, candidates).map(SharedString::new_static);
        Self {
            ui: pick(UI),
            mono: pick(MONO),
            display: pick(DISPLAY),
            terminal: pick(MONO),
        }
    }

    /// [`Fonts::resolve`] with the families the settings name for the window
    /// and the terminals first (RF-CFG-02); paths and branches in the bars
    /// keep [`Fonts::mono`]. A family that is not installed falls back to the
    /// role's list, with a notice saying so.
    pub(crate) fn configured(
        installed: &[String],
        ui: Option<&str>,
        terminal: Option<&str>,
    ) -> (Self, Vec<String>) {
        let mut fonts = Self::resolve(installed);
        let mut notices = Vec::new();
        for (wanted, slot, role) in [
            (ui, &mut fonts.ui, "the window uses"),
            (terminal, &mut fonts.terminal, "the terminals use"),
        ] {
            let Some(wanted) = wanted else { continue };
            match installed.iter().find(|i| i.eq_ignore_ascii_case(wanted)) {
                Some(found) => *slot = Some(SharedString::from(found.clone())),
                None => notices.push(format!(
                    "The font \"{wanted}\" is not installed; {role} {}.",
                    slot.as_deref().unwrap_or("the default font")
                )),
            }
        }
        (fonts, notices)
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
    fn rf_cfg_02_a_configured_family_comes_first_in_its_installed_spelling() {
        let all = installed(&["Consolas", "Fira Code", "Segoe UI", "Yu Gothic UI"]);
        let (fonts, notices) = Fonts::configured(&all, Some("segoe ui"), Some("Fira Code"));
        assert_eq!(fonts.ui.as_deref(), Some("Segoe UI"));
        assert_eq!(fonts.terminal.as_deref(), Some("Fira Code"));
        assert_eq!(
            fonts.mono.as_deref(),
            Some("Consolas"),
            "the bars keep theirs"
        );
        assert!(notices.is_empty());
        let (fonts, notices) = Fonts::configured(&all, None, None);
        assert_eq!(fonts, Fonts::resolve(&all));
        assert!(notices.is_empty());
    }

    #[test]
    fn rf_cfg_02_a_missing_family_falls_back_with_a_notice() {
        let all = installed(&["Consolas", "Yu Gothic UI"]);
        let (fonts, notices) = Fonts::configured(&all, Some("Nope Sans"), Some("Nope Mono"));
        assert_eq!(fonts, Fonts::resolve(&all));
        assert_eq!(
            notices,
            [
                "The font \"Nope Sans\" is not installed; the window uses Yu Gothic UI.",
                "The font \"Nope Mono\" is not installed; the terminals use Consolas.",
            ]
        );
        let (_, notices) = Fonts::configured(&[], Some("Nope Sans"), None);
        assert_eq!(
            notices,
            ["The font \"Nope Sans\" is not installed; the window uses the default font."]
        );
    }

    #[test]
    fn nothing_installed_leaves_the_default_family() {
        assert_eq!(Fonts::resolve(&[]), Fonts::default());
    }
}
