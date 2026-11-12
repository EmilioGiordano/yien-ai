//! What the status bar's right side says: the hints, or a notice that
//! replaces them (RF-NAV-03, RN-01).

/// The commands whose keys the bar hints at, with the words after them.
pub(super) const HINTED: [(&str, &str); 3] = [
    ("palette.open", "commands"),
    ("switcher.open", "go to"),
    ("region.next", "move focus"),
];

/// What the right side of the bar shows.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Line {
    Hints(String),
    Notice { text: String, error: bool },
}

/// The hints for the commands that have keys now; `keys` answers with a
/// command's current keys, `None` when it is unbound (RF-NAV-03, RF-CFG-03).
pub(super) fn hints(keys: impl Fn(&str) -> Option<String>) -> Vec<String> {
    HINTED
        .iter()
        .filter_map(|(id, words)| keys(id).map(|keys| format!("{keys} {words}")))
        .collect()
}

/// A notice replaces the hints while it shows, so it is never squeezed out
/// by them; an error differs by weight, never by red (RN-01).
pub(super) fn line(notice: Option<(&str, bool)>, hints: &[String]) -> Option<Line> {
    match notice {
        Some((text, error)) => Some(Line::Notice {
            text: text.to_owned(),
            error,
        }),
        None if hints.is_empty() => None,
        None => Some(Line::Hints(hints.join(" · "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymap(bound: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |id| {
            bound
                .iter()
                .find(|(bound_id, _)| *bound_id == id)
                .map(|(_, keys)| (*keys).to_owned())
        }
    }

    #[test]
    fn rf_nav_03_default_keys_give_the_three_hints() {
        let shown = hints(keymap(&[
            ("palette.open", "Ctrl+Shift+P"),
            ("switcher.open", "Ctrl+Shift+O"),
            ("region.next", "F6"),
        ]));
        assert_eq!(
            line(None, &shown),
            Some(Line::Hints(
                "Ctrl+Shift+P commands · Ctrl+Shift+O go to · F6 move focus".to_owned()
            ))
        );
    }

    #[test]
    fn rf_cfg_03_a_rebound_key_shows_its_new_label() {
        let shown = hints(keymap(&[
            ("palette.open", "Ctrl+Shift+Y"),
            ("switcher.open", "Ctrl+Shift+O"),
            ("region.next", "F6"),
        ]));
        assert_eq!(shown[0], "Ctrl+Shift+Y commands");
    }

    #[test]
    fn rf_cfg_03_an_unbound_command_has_no_hint() {
        let shown = hints(keymap(&[
            ("palette.open", "Ctrl+Shift+P"),
            ("region.next", "F6"),
        ]));
        assert_eq!(shown, ["Ctrl+Shift+P commands", "F6 move focus"]);
        assert_eq!(line(None, &hints(keymap(&[]))), None);
    }

    #[test]
    fn rn_01_a_notice_replaces_the_hints_and_an_error_is_marked() {
        let shown = hints(keymap(&[("region.next", "F6")]));
        assert_eq!(
            line(Some(("Saved.", false)), &shown),
            Some(Line::Notice {
                text: "Saved.".to_owned(),
                error: false
            })
        );
        assert_eq!(
            line(Some(("Failed.", true)), &shown),
            Some(Line::Notice {
                text: "Failed.".to_owned(),
                error: true
            })
        );
    }
}
