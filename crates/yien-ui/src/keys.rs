//! The settings file's `[keys]` table against the catalog (RF-CFG-03):
//! which commands get which chords, and what to tell the user about ids
//! that do not exist, chords that do not parse and chords two commands share.

use std::collections::HashMap;

use gpui_kit::{Keystroke, Modifiers};
use yien_config::KeyOverride;

use crate::catalog::{CATALOG, CommandSpec, Scope, keystroke_label};

/// Chords by command id, replacing that command's default keys; an empty
/// list unbinds it.
pub(crate) type Rebinds = HashMap<&'static str, Vec<String>>;

const FILE: &str = yien_config::FILE_NAME;

/// A chord's steps, compared by modifiers and key only.
type Chord = Vec<(Modifiers, String)>;

/// The rebinds the table asks for and the notices it gives. Entries with an
/// unknown id and chords that do not parse are left out.
pub(crate) fn plan(table: &[KeyOverride]) -> (Rebinds, Vec<String>) {
    let mut rebinds = Rebinds::new();
    let mut lines = HashMap::new();
    let mut notices = Vec::new();
    for entry in table {
        let Some(spec) = CATALOG.iter().find(|s| s.id == entry.command) else {
            notices.push(format!(
                "{FILE}: line {}: there is no command {}, ignored.",
                entry.line, entry.command
            ));
            continue;
        };
        let mut chords = Vec::new();
        for chord in &entry.chords {
            if parse_chord(chord).is_some() {
                chords.push(chord.trim().to_owned());
            } else {
                notices.push(format!(
                    "{FILE}: line {}: \"{chord}\" is not a key chord, ignored.",
                    entry.line
                ));
            }
        }
        rebinds.insert(spec.id, chords);
        lines.insert(spec.id, entry.line);
    }
    notices.extend(conflicts(&rebinds, &lines));
    (rebinds, notices)
}

/// Pairs of commands that end up on the same chord where both apply, when
/// the table caused it. The one gpui runs is named: a terminal command beats
/// a window one, and between equals the later in the catalog wins.
fn conflicts(rebinds: &Rebinds, lines: &HashMap<&'static str, usize>) -> Vec<String> {
    let effective: Vec<(&CommandSpec, Vec<Chord>)> = CATALOG
        .iter()
        .map(|spec| {
            let chords: Vec<&str> = match rebinds.get(spec.id) {
                Some(chords) => chords.iter().map(String::as_str).collect(),
                None => spec.keys.to_vec(),
            };
            (spec, chords.into_iter().filter_map(parse_chord).collect())
        })
        .collect();
    let mut notices = Vec::new();
    for (at, (a, a_chords)) in effective.iter().enumerate() {
        for (b, b_chords) in &effective[at + 1..] {
            if !rebinds.contains_key(a.id) && !rebinds.contains_key(b.id) {
                continue;
            }
            for chord in a_chords.iter().filter(|c| b_chords.contains(c)) {
                let winner = if a.scope == Scope::Terminal && b.scope == Scope::Window {
                    a.id
                } else {
                    b.id
                };
                let line = lines
                    .get(b.id)
                    .or_else(|| lines.get(a.id))
                    .copied()
                    .unwrap_or(1);
                notices.push(format!(
                    "{FILE}: line {line}: {} runs both {} and {}; only {winner} will.",
                    label(chord),
                    a.id,
                    b.id
                ));
            }
        }
    }
    notices
}

fn parse_chord(chord: &str) -> Option<Chord> {
    let steps: Vec<(Modifiers, String)> = chord
        .split_whitespace()
        .map(|step| Keystroke::parse(step).map(|k| (k.modifiers, k.key)))
        .collect::<Result<_, _>>()
        .ok()?;
    (!steps.is_empty()).then_some(steps)
}

fn label(chord: &[(Modifiers, String)]) -> String {
    chord
        .iter()
        .map(|(modifiers, key)| {
            keystroke_label(&Keystroke {
                modifiers: *modifiers,
                key: key.clone(),
                key_char: None,
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Vec<KeyOverride> {
        yien_config::parse(text).unwrap().config.keys
    }

    #[test]
    fn rf_cfg_03_known_commands_get_their_new_chords() {
        let (rebinds, notices) = plan(&table(
            "[keys]\n\"pane.split_right\" = \"ctrl-alt-r\"\n\"pane.close\" = []\n",
        ));
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(rebinds["pane.split_right"], ["ctrl-alt-r"]);
        assert!(rebinds["pane.close"].is_empty(), "unbound");
        assert_eq!(rebinds.len(), 2);
    }

    #[test]
    fn rf_cfg_03_unknown_ids_and_bad_chords_are_reported_with_their_line() {
        let (rebinds, notices) = plan(&table(
            "[keys]\n\"pane.nope\" = \"f1\"\n\"pane.close\" = [\"ctrl-w\", \"ctrl-a-b\"]\n",
        ));
        assert_eq!(rebinds["pane.close"], ["ctrl-w"]);
        assert!(!rebinds.contains_key("pane.nope"));
        assert_eq!(
            notices,
            [
                "config.toml: line 2: there is no command pane.nope, ignored.",
                "config.toml: line 3: \"ctrl-a-b\" is not a key chord, ignored.",
            ]
        );
    }

    #[test]
    fn rf_cfg_03_a_chord_another_command_keeps_is_a_conflict() {
        // Ctrl+Shift+P is the palette's default key.
        let (_, notices) = plan(&table("[keys]\n\"theme.toggle\" = \"shift-ctrl-p\"\n"));
        assert_eq!(
            notices,
            [
                "config.toml: line 2: Ctrl+Shift+P runs both palette.open and theme.toggle; \
                 only theme.toggle will."
            ]
        );
        // Moving the palette away first leaves no conflict.
        let (_, notices) = plan(&table(
            "[keys]\n\"palette.open\" = \"f1\"\n\"theme.toggle\" = \"ctrl-shift-p\"\n",
        ));
        assert!(notices.is_empty(), "{notices:?}");
    }

    #[test]
    fn rf_cfg_03_a_terminal_command_wins_over_a_window_one() {
        let (_, notices) = plan(&table("[keys]\n\"tab.new\" = \"ctrl-shift-c\"\n"));
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].ends_with("only terminal.copy will."),
            "{notices:?}"
        );
    }

    #[test]
    fn rf_cfg_03_the_default_keys_have_no_conflicts() {
        let everything: Vec<KeyOverride> = CATALOG
            .iter()
            .enumerate()
            .map(|(line, spec)| KeyOverride {
                command: spec.id.to_owned(),
                chords: spec.keys.iter().map(|k| k.to_string()).collect(),
                line,
            })
            .collect();
        let (_, notices) = plan(&everything);
        assert!(notices.is_empty(), "{notices:?}");
    }
}
