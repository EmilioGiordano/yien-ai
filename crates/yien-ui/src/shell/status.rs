//! What the status bar's right side says: the hints, or a notice that
//! replaces them (RF-CFG-03).

use std::time::Duration;

use gpui_kit::Keymap;

use crate::catalog::{shortcut, spec};

/// The commands whose keys the bar hints at, with the words after them.
pub(super) const HINTED: [(&str, &str); 3] = [
    ("palette.open", "commands"),
    ("switcher.open", "go to"),
    ("region.next", "move focus"),
];

/// How long a plain notice stays up; a warning or an error stays until
/// dismissed.
pub(super) const INFO_LIFETIME: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    Info,
    Warning,
    Error,
}

/// A message the bar shows in place of the hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) kind: NoticeKind,
    /// Stays until dismissed or replaced, whatever its kind.
    pub(crate) lasting: bool,
}

impl Notice {
    pub(crate) fn new(text: impl Into<String>, kind: NoticeKind) -> Self {
        Self {
            text: text.into(),
            kind,
            lasting: false,
        }
    }

    pub(crate) fn info(text: impl Into<String>) -> Self {
        Self::new(text, NoticeKind::Info)
    }

    /// A plain notice that stays until dismissed, such as a newer YIEN
    /// being available.
    pub(crate) fn lasting_info(text: impl Into<String>) -> Self {
        Self {
            lasting: true,
            ..Self::info(text)
        }
    }

    pub(crate) fn warning(text: impl Into<String>) -> Self {
        Self::new(text, NoticeKind::Warning)
    }

    pub(crate) fn error(text: impl Into<String>) -> Self {
        Self::new(text, NoticeKind::Error)
    }

    /// How long it stays up by itself.
    pub(crate) fn lifetime(&self) -> Option<Duration> {
        if self.lasting {
            None
        } else {
            lifetime(self.kind)
        }
    }
}

/// How long a notice of this kind stays up by itself.
pub(super) fn lifetime(kind: NoticeKind) -> Option<Duration> {
    match kind {
        NoticeKind::Info => Some(INFO_LIFETIME),
        NoticeKind::Warning | NoticeKind::Error => None,
    }
}

/// The kind of a notice a settings reading puts up: a reading that failed
/// is an error; one that loaded and still has something to say (a warning,
/// or the start's profile problems) is a warning.
pub(super) fn reading_kind(loaded: bool) -> NoticeKind {
    if loaded {
        NoticeKind::Warning
    } else {
        NoticeKind::Error
    }
}

/// Whether the timer set for notice number `shown` takes down the notice
/// showing now, number `current`: only a plain notice, and never a newer one.
pub(super) fn timer_clears(showing: Option<&Notice>, shown: u64, current: u64) -> bool {
    showing.is_some_and(|notice| notice.lifetime().is_some()) && shown == current
}

/// The notice to bring back once `new` goes: a plain notice that only
/// shows for a moment covers a standing one (a warning, an error or a
/// lasting notice) without taking it away; a standing notice replaces
/// everything.
pub(super) fn behind(
    current: Option<Notice>,
    behind: Option<Notice>,
    new: &Notice,
) -> Option<Notice> {
    new.lifetime()?;
    match current {
        Some(current) if current.lifetime().is_none() => Some(current),
        _ => behind,
    }
}

/// What the right side of the bar shows.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Line {
    Hints(String),
    Notice { text: String, error: bool },
}

/// The hints for the commands that have keys now; `keys` answers with a
/// command's current keys, `None` when it is unbound (RF-CFG-03).
pub(super) fn hints(keys: impl Fn(&str) -> Option<String>) -> Vec<String> {
    HINTED
        .iter()
        .filter_map(|(id, words)| keys(id).map(|keys| format!("{keys} {words}")))
        .collect()
}

/// The hints for what `keymap` binds now.
pub(super) fn live_hints(keymap: &Keymap) -> Vec<String> {
    hints(|id| shortcut(keymap, (spec(id)?.action)().as_ref()))
}

/// A notice replaces the hints while it shows, so it is never squeezed out
/// by them; an error differs by weight, never by red (RN-01).
pub(super) fn line(notice: Option<&Notice>, hints: &[String]) -> Option<Line> {
    match notice {
        Some(notice) => Some(Line::Notice {
            text: notice.text.clone(),
            error: notice.kind == NoticeKind::Error,
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
    fn rf_cfg_03_default_keys_give_the_three_hints() {
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
    fn a_notice_replaces_the_hints_and_an_error_is_marked() {
        let shown = hints(keymap(&[("region.next", "F6")]));
        assert_eq!(
            line(Some(&Notice::info("Saved.")), &shown),
            Some(Line::Notice {
                text: "Saved.".to_owned(),
                error: false
            })
        );
        assert_eq!(
            line(Some(&Notice::error("Failed.")), &shown),
            Some(Line::Notice {
                text: "Failed.".to_owned(),
                error: true
            })
        );
    }

    #[test]
    fn a_plain_notice_clears_itself_and_an_error_stays() {
        assert_eq!(lifetime(NoticeKind::Info), Some(Duration::from_secs(5)));
        assert_eq!(lifetime(NoticeKind::Error), None);
        assert!(timer_clears(Some(&Notice::info("a")), 3, 3));
        assert!(!timer_clears(Some(&Notice::error("a")), 3, 3));
        assert!(!timer_clears(None, 3, 3));
    }

    #[test]
    fn a_warning_stays_and_a_failed_reading_is_an_error() {
        assert_eq!(lifetime(NoticeKind::Warning), None);
        assert!(!timer_clears(Some(&Notice::warning("a")), 3, 3));
        assert_eq!(reading_kind(true), NoticeKind::Warning);
        assert_eq!(reading_kind(false), NoticeKind::Error);
    }

    #[test]
    fn a_lasting_plain_notice_stays_and_is_not_an_error() {
        let notice = Notice::lasting_info("YIEN 0.2.0 is available");
        assert_eq!(notice.kind, NoticeKind::Info);
        assert_eq!(notice.lifetime(), None);
        assert!(!timer_clears(Some(&notice), 3, 3));
        assert_eq!(
            line(Some(&notice), &[]),
            Some(Line::Notice {
                text: "YIEN 0.2.0 is available".to_owned(),
                error: false
            })
        );
    }

    #[test]
    fn a_stale_timer_does_not_clear_a_newer_notice() {
        assert!(!timer_clears(Some(&Notice::info("a")), 3, 4));
    }

    #[test]
    fn rf_cfg_03_the_real_default_keymap_gives_the_prototype_line() {
        let keymap = Keymap::new(crate::actions::bindings());
        let shown = live_hints(&keymap);
        assert_eq!(
            line(None, &shown),
            Some(Line::Hints(
                "Ctrl+Shift+P commands · Ctrl+Shift+O go to · F6 move focus".to_owned()
            ))
        );
    }

    #[test]
    fn rf_cfg_03_the_real_keymap_follows_a_rebind_and_an_unbind() {
        let (rebinds, notices) = crate::keys::plan(
            &yien_config::parse(
                "[keys]
\"palette.open\" = \"ctrl-alt-y\"
\"region.next\" = []
",
            )
            .unwrap()
            .config
            .keys,
        );
        assert!(notices.is_empty(), "{notices:?}");
        let keymap = Keymap::new(crate::actions::bindings_with(&rebinds));
        assert_eq!(
            live_hints(&keymap),
            ["Ctrl+Alt+Y commands", "Ctrl+Shift+O go to"]
        );
    }

    #[test]
    fn a_passing_notice_never_takes_away_a_standing_one() {
        let warning = Notice::warning("config.toml: line 3");
        let update = Notice::lasting_info("YIEN 0.2.0 is available");
        let info = Notice::info("Notifications muted.");
        assert_eq!(
            behind(Some(warning.clone()), None, &info),
            Some(warning.clone())
        );
        assert_eq!(
            behind(Some(update.clone()), None, &info),
            Some(update.clone())
        );
        assert_eq!(
            behind(Some(Notice::info("a")), Some(update.clone()), &info),
            Some(update.clone()),
            "one passing notice after another keeps the standing one"
        );
        assert_eq!(behind(Some(update), Some(warning.clone()), &warning), None);
        assert_eq!(behind(None, None, &info), None);
    }
}
