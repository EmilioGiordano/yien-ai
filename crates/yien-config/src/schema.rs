//! Every key the file accepts, its default and what it means (plan §7.4).
//! The parser checks keys against this table and the first-run file is
//! written from it, so the two cannot drift apart.

/// One key of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    /// The dotted key, as it may be written at the top of the file.
    pub key: &'static str,
    /// The default, as a TOML value.
    pub default: &'static str,
    /// What the key does and what it accepts.
    pub doc: &'static str,
}

/// Every key, in the order the first-run file lists them. `[keys]` is
/// separate: its keys are the catalog's command ids.
pub const SCHEMA: &[Setting] = &[
    Setting {
        key: "theme",
        default: "\"sumi\"",
        doc: "\"sumi\" (dark), \"washi\" (light) or \"system\" (follows the OS).",
    },
    Setting {
        key: "ui.font.family",
        default: "\"\"",
        doc: "The window's font. Empty: the first installed of Zen Kaku Gothic New, \
              Yu Gothic UI, Segoe UI.",
    },
    Setting {
        key: "ui.font.size",
        default: "13",
        doc: "The window's text size in pixels, 8 to 20.",
    },
    Setting {
        key: "ui.tabs.group",
        default: "\"project\"",
        doc: "How the tab bar lists tabs: \"project\" (under their projects) or \
              \"state\" (needs you, error, done, working, idle). The palette switches it \
              until the file is next saved.",
    },
    Setting {
        key: "terminal.font.family",
        default: "\"\"",
        doc: "The terminals' font; use a monospaced one. Empty: the first installed of \
              M PLUS 1 Code, Cascadia Mono, Consolas.",
    },
    Setting {
        key: "terminal.font.size",
        default: "13",
        doc: "The terminals' text size in pixels, 8 to 32.",
    },
    Setting {
        key: "editor",
        default: "\"\"",
        doc: "The command that opens files, such as \"code\" or \"notepad\". Empty: the \
              OS default for the file type.",
    },
];

/// Tables that only group other keys.
pub(crate) const TABLES: &[&str] = &["ui", "ui.font", "ui.tabs", "terminal", "terminal.font"];

/// The table that maps command ids to chords (RF-CFG-03).
pub(crate) const KEYS_TABLE: &str = "keys";

/// The setting with this dotted key.
pub(crate) fn setting(key: &str) -> Option<&'static Setting> {
    SCHEMA.iter().find(|s| s.key == key)
}

/// The file written on first run: every setting commented out at its
/// default, so the file only holds what the user changes (plan §7.4).
/// `commands` are the catalog's `(id, title)` pairs, listed for `[keys]`.
/// The file is never rewritten, so that list goes stale as the catalog grows;
/// the file says so.
///
/// ```
/// let text = yien_config::default_file(&[("pane.close", "pane: close")]);
/// assert!(text.contains("# theme = \"sumi\""));
/// assert!(text.contains("pane.close"));
/// assert_eq!(yien_config::parse(&text).unwrap().config, Default::default());
/// ```
pub fn default_file(commands: &[(&str, &str)]) -> String {
    let mut out = String::from(
        "# YIEN settings. Changes apply as soon as the file is saved; no restart.\n\
         # Remove the \"#\" in front of a setting to change it. An invalid file is\n\
         # ignored, the last valid settings stay, and the status bar names the line.\n\n",
    );
    for s in SCHEMA {
        out.push_str(&format!("# {}\n# {} = {}\n\n", s.doc, s.key, s.default));
    }
    out.push_str(
        "# Keys: a command id and the chords that run it, replacing its default ones.\n\
         # An empty list unbinds the command. Chords are written as ctrl, alt, shift\n\
         # and win joined to the key by \"-\", with a space between the steps of a sequence.\n\
         # [keys]\n\
         # \"pane.split_right\" = \"alt-+\"\n\
         # \"theme.toggle\" = [\"ctrl-shift-l\", \"f12\"]\n",
    );
    if !commands.is_empty() {
        out.push_str(
            "\n# Command ids, as of the YIEN that wrote this file; newer versions may add\n\
             # more, and this file is never rewritten to list them.\n",
        );
        let width = commands.iter().map(|(id, _)| id.len()).max().unwrap_or(0);
        for (id, title) in commands {
            out.push_str(&format!("#   {id:<width$}  {title}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_cfg_01_the_first_run_file_holds_only_comments() {
        let text = default_file(&[("pane.close", "pane: close")]);
        for line in text.lines() {
            assert!(line.is_empty() || line.starts_with('#'), "{line}");
        }
        let loaded = crate::parse(&text).unwrap();
        assert_eq!(loaded.config, crate::Config::default());
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn rf_cfg_01_every_commented_default_parses_back_to_the_default() {
        // Uncomment each setting line (not the prose) and parse the result.
        let uncommented: String = default_file(&[])
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .filter(|l| {
                SCHEMA
                    .iter()
                    .any(|s| l.starts_with(&format!("{} = ", s.key)))
            })
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(uncommented.lines().count(), SCHEMA.len());
        let loaded = crate::parse(&uncommented).unwrap();
        assert_eq!(loaded.config, crate::Config::default());
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    }

    #[test]
    fn rf_cfg_03_the_keys_example_parses_when_uncommented() {
        let text = default_file(&[]);
        let start = text.find("# [keys]").unwrap();
        let keys: String = text[start..]
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .map(|l| format!("{l}\n"))
            .collect();
        let loaded = crate::parse(&keys).unwrap();
        assert_eq!(loaded.config.keys.len(), 2);
        assert_eq!(loaded.config.keys[1].chords, ["ctrl-shift-l", "f12"]);
    }

    #[test]
    fn every_dotted_key_sits_in_a_known_table() {
        for s in SCHEMA {
            if let Some((table, _)) = s.key.rsplit_once('.') {
                assert!(TABLES.contains(&table), "{}", s.key);
            }
        }
    }
}
