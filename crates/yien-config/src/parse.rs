//! From the file's text to a [`Config`], walking TOML with the spans of every
//! key and value so each problem names its line.

use std::ops::Range;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::schema::{self, KEYS_TABLE, TABLES};
use crate::{
    KeyOverride, Loaded, MAX_FILE_BYTES, MAX_FONT_SIZE, MAX_UI_FONT_SIZE, MIN_FONT_SIZE, Problem,
    TabGrouping, ThemeSetting,
};

/// Parses and checks the file. An `Err` is the first problem that makes the
/// file unusable; the caller keeps its last valid settings. Unknown keys and
/// out-of-range sizes are accepted with a warning.
///
/// ```
/// let loaded = yien_config::parse("ui.font.size = 99\ncolour = 1\n").unwrap();
/// assert_eq!(loaded.config.ui_font.size, yien_config::MAX_UI_FONT_SIZE);
/// let lines: Vec<usize> = loaded.warnings.iter().map(|w| w.line).collect();
/// assert_eq!(lines, [1, 2]);
/// ```
pub fn parse(text: &str) -> Result<Loaded, Problem> {
    if text.len() > MAX_FILE_BYTES {
        return Err(Problem::at(
            text,
            0,
            format!("the file is larger than {} KiB", MAX_FILE_BYTES / 1024),
        ));
    }
    let root = DeTable::parse(text).map_err(|e| {
        let at = e.span().map_or(0, |s| s.start);
        Problem::at(text, at, e.message().to_owned())
    })?;
    let mut walk = Walk {
        text,
        loaded: Loaded::default(),
    };
    walk.table("", root.get_ref())?;
    walk.loaded.warnings.sort_by_key(|w| (w.line, w.column));
    Ok(walk.loaded)
}

struct Walk<'t> {
    text: &'t str,
    loaded: Loaded,
}

impl Walk<'_> {
    fn problem(&self, span: Range<usize>, message: impl Into<String>) -> Problem {
        Problem::at(self.text, span.start, message)
    }

    fn warn(&mut self, span: Range<usize>, message: impl Into<String>) {
        let p = self.problem(span, message);
        self.loaded.warnings.push(p);
    }

    fn table(&mut self, prefix: &str, table: &DeTable<'_>) -> Result<(), Problem> {
        for (key, value) in table {
            let path = if prefix.is_empty() {
                key.get_ref().to_string()
            } else {
                format!("{prefix}.{}", key.get_ref())
            };
            if path == KEYS_TABLE {
                self.keys(value)?;
            } else if TABLES.contains(&path.as_str()) {
                match value.get_ref() {
                    DeValue::Table(inner) => self.table(&path, inner)?,
                    other => {
                        return Err(self.problem(
                            value.span(),
                            format!("{path} must be a table, not {}", other.type_str()),
                        ));
                    }
                }
            } else if schema::setting(&path).is_some() {
                self.setting(&path, value)?;
            } else {
                self.warn(key.span(), format!("unknown setting {path}, ignored"));
            }
        }
        Ok(())
    }

    fn setting(&mut self, path: &str, value: &Spanned<DeValue<'_>>) -> Result<(), Problem> {
        match path {
            "theme" => {
                let name = self.string(path, value)?;
                self.loaded.config.theme = match name.to_ascii_lowercase().as_str() {
                    "sumi" => ThemeSetting::Sumi,
                    "washi" => ThemeSetting::Washi,
                    "system" => ThemeSetting::System,
                    _ => {
                        return Err(self.problem(
                            value.span(),
                            format!(
                                "theme must be \"sumi\", \"washi\" or \"system\", not \"{name}\""
                            ),
                        ));
                    }
                };
            }
            "editor" => self.loaded.config.editor = non_empty(self.string(path, value)?),
            "ui.tabs.group" => {
                let name = self.string(path, value)?;
                self.loaded.config.tab_grouping = match name.to_ascii_lowercase().as_str() {
                    "project" => TabGrouping::Project,
                    "state" => TabGrouping::State,
                    _ => {
                        return Err(self.problem(
                            value.span(),
                            format!(
                                "ui.tabs.group must be \"project\" or \"state\", not \"{name}\""
                            ),
                        ));
                    }
                };
            }
            "ui.font.family" => {
                self.loaded.config.ui_font.family = non_empty(self.string(path, value)?);
            }
            "terminal.font.family" => {
                self.loaded.config.terminal_font.family = non_empty(self.string(path, value)?);
            }
            "terminal.shell_integration" => {
                self.loaded.config.shell_integration = self.boolean(path, value)?;
            }
            "ui.font.size" => {
                self.loaded.config.ui_font.size = self.size(path, value, MAX_UI_FONT_SIZE)?;
            }
            "terminal.font.size" => {
                self.loaded.config.terminal_font.size = self.size(path, value, MAX_FONT_SIZE)?;
            }
            // `schema::setting` accepted the path, so this is a key added to
            // the schema without a branch here; the schema test catches it.
            _ => self.warn(value.span(), format!("{path} is not handled, ignored")),
        }
        Ok(())
    }

    fn string(&self, path: &str, value: &Spanned<DeValue<'_>>) -> Result<String, Problem> {
        match value.get_ref() {
            DeValue::String(s) => Ok(s.to_string()),
            other => Err(self.problem(
                value.span(),
                format!("{path} must be a string, not {}", other.type_str()),
            )),
        }
    }

    fn boolean(&self, path: &str, value: &Spanned<DeValue<'_>>) -> Result<bool, Problem> {
        match value.get_ref() {
            DeValue::Boolean(b) => Ok(*b),
            other => Err(self.problem(
                value.span(),
                format!("{path} must be true or false, not {}", other.type_str()),
            )),
        }
    }

    /// A size as written, integer or float, brought into range with a warning.
    fn size(&mut self, path: &str, value: &Spanned<DeValue<'_>>, max: f32) -> Result<f32, Problem> {
        let number = match value.get_ref() {
            DeValue::Integer(i) => i64::from_str_radix(i.as_str(), i.radix())
                .ok()
                .map(|n| n as f64),
            DeValue::Float(f) => f.as_str().parse::<f64>().ok(),
            other => {
                return Err(self.problem(
                    value.span(),
                    format!("{path} must be a number, not {}", other.type_str()),
                ));
            }
        };
        let Some(number) = number.filter(|n| n.is_finite()) else {
            return Err(self.problem(value.span(), format!("{path} must be a finite number")));
        };
        let (min, max) = (f64::from(MIN_FONT_SIZE), f64::from(max));
        let kept = number.clamp(min, max);
        if kept != number {
            self.warn(
                value.span(),
                format!("{path} {number} is out of range, {kept} is used ({min} to {max})"),
            );
        }
        Ok(kept as f32)
    }

    fn keys(&mut self, value: &Spanned<DeValue<'_>>) -> Result<(), Problem> {
        let DeValue::Table(table) = value.get_ref() else {
            return Err(self.problem(
                value.span(),
                format!("keys must be a table, not {}", value.get_ref().type_str()),
            ));
        };
        let mut entries = Vec::new();
        for (command, chords) in table {
            let path = format!("keys.{}", command.get_ref());
            let list = match chords.get_ref() {
                DeValue::String(s) => vec![s.to_string()],
                DeValue::Array(items) => {
                    let mut list = Vec::new();
                    for item in items.iter() {
                        match item.get_ref() {
                            DeValue::String(s) => list.push(s.to_string()),
                            other => {
                                return Err(self.problem(
                                    item.span(),
                                    format!(
                                        "{path} lists chords as strings, not {}",
                                        other.type_str()
                                    ),
                                ));
                            }
                        }
                    }
                    list
                }
                other => {
                    return Err(self.problem(
                        chords.span(),
                        format!(
                            "{path} must be a chord or a list of chords, not {}",
                            other.type_str()
                        ),
                    ));
                }
            };
            let start = command.span().start;
            entries.push((
                start,
                KeyOverride {
                    command: command.get_ref().to_string(),
                    chords: list,
                    line: Problem::at(self.text, start, "").line,
                },
            ));
        }
        entries.sort_by_key(|(start, _)| *start);
        self.loaded.config.keys = entries.into_iter().map(|(_, k)| k).collect();
        Ok(())
    }
}

fn non_empty(s: String) -> Option<String> {
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontSetting;

    fn ok(text: &str) -> Loaded {
        parse(text).unwrap_or_else(|p| panic!("{p}"))
    }

    fn err(text: &str) -> Problem {
        parse(text).expect_err("the file should be invalid")
    }

    #[test]
    fn rf_cfg_02_a_good_file_sets_every_key() {
        let loaded = ok(r#"
theme = "washi"
editor = "code --reuse-window"

[ui.font]
family = "Segoe UI"
size = 14.5

[terminal]
font.family = "Cascadia Mono"
font.size = 18

[keys]
"pane.split_right" = "ctrl-alt-r"
"theme.toggle" = ["ctrl-shift-l", "f12"]
"pane.close" = []
"#);
        let c = loaded.config;
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(c.theme, ThemeSetting::Washi);
        assert_eq!(c.editor.as_deref(), Some("code --reuse-window"));
        assert_eq!(
            c.ui_font,
            FontSetting {
                family: Some("Segoe UI".into()),
                size: 14.5
            }
        );
        assert_eq!(c.terminal_font.family.as_deref(), Some("Cascadia Mono"));
        assert_eq!(c.terminal_font.size, 18.0);
        let keys: Vec<(&str, usize, usize)> = c
            .keys
            .iter()
            .map(|k| (k.command.as_str(), k.chords.len(), k.line))
            .collect();
        assert_eq!(
            keys,
            [
                ("pane.split_right", 1, 14),
                ("theme.toggle", 2, 15),
                ("pane.close", 0, 16)
            ]
        );
    }

    #[test]
    fn rf_cfg_01_an_empty_file_is_the_defaults() {
        assert_eq!(ok(""), Loaded::default());
        assert_eq!(ok("# only a comment\n\n"), Loaded::default());
    }

    #[test]
    fn rf_cfg_02_theme_names_ignore_case_and_system_is_accepted() {
        assert_eq!(ok("theme = \"Washi\"").config.theme, ThemeSetting::Washi);
        assert_eq!(ok("theme = \"system\"").config.theme, ThemeSetting::System);
    }

    #[test]
    fn rf_atn_07_the_tab_grouping_is_project_or_state() {
        assert_eq!(ok("").config.tab_grouping, TabGrouping::Project);
        assert_eq!(
            ok("ui.tabs.group = \"State\"").config.tab_grouping,
            TabGrouping::State
        );
        assert_eq!(
            ok("[ui.tabs]
group = \"project\"")
            .config
            .tab_grouping,
            TabGrouping::Project
        );
        let p = err("ui.tabs.group = \"status\"
");
        assert_eq!((p.line, p.column), (1, 17));
        assert!(
            p.message.contains("\"project\" or \"state\""),
            "{}",
            p.message
        );
    }

    #[test]
    fn rf_ses_01_shell_integration_is_on_unless_turned_off() {
        assert!(ok("").config.shell_integration);
        assert!(
            ok("terminal.shell_integration = true")
                .config
                .shell_integration
        );
        let off = ok("[terminal]\nshell_integration = false");
        assert!(!off.config.shell_integration);
        assert!(off.warnings.is_empty(), "{:?}", off.warnings);
        let p = err("\nterminal.shell_integration = \"no\"\n");
        assert_eq!((p.line, p.column), (2, 30));
        assert!(
            p.message
                .contains("terminal.shell_integration must be true or false, not string"),
            "{p}"
        );
    }

    #[test]
    fn rf_cfg_01_a_bad_value_names_its_line_and_column() {
        let p = err("theme = \"sumi\"\n\nterminal.font.size = \"big\"\n");
        assert_eq!((p.line, p.column), (3, 22));
        assert!(
            p.message.contains("terminal.font.size must be a number"),
            "{p}"
        );

        let p = err("theme = \"blue\"\n");
        assert_eq!((p.line, p.column), (1, 9));
        assert!(p.message.contains("not \"blue\""), "{p}");

        let p = err("[ui]\nfont = 3\n");
        assert_eq!(p.line, 2);
        assert!(p.message.contains("ui.font must be a table"), "{p}");

        let p = err("editor = 1\n");
        assert!(
            p.message.contains("editor must be a string, not integer"),
            "{p}"
        );
    }

    #[test]
    fn rf_cfg_01_a_syntax_error_names_its_line() {
        let p = err("theme = \"washi\"\nui.font.size = = 3\n");
        assert_eq!(p.line, 2, "{p}");
        let p = err("theme = \"washi\"\ntheme = \"sumi\"\n");
        assert_eq!(p.line, 2, "a duplicate key: {p}");
        let p = err("[keys\n");
        assert_eq!(p.line, 1, "{p}");
    }

    #[test]
    fn rf_cfg_01_unknown_keys_warn_and_the_rest_applies() {
        let loaded =
            ok("colour = \"red\"\ntheme = \"washi\"\n[ui.font]\nweight = 700\n[extra]\na = 1\n");
        assert_eq!(loaded.config.theme, ThemeSetting::Washi);
        let warnings: Vec<(usize, &str)> = loaded
            .warnings
            .iter()
            .map(|w| (w.line, w.message.as_str()))
            .collect();
        assert_eq!(
            warnings,
            [
                (1, "unknown setting colour, ignored"),
                (4, "unknown setting ui.font.weight, ignored"),
                (5, "unknown setting extra, ignored"),
            ]
        );
    }

    #[test]
    fn rf_cfg_02_font_sizes_are_clamped_with_a_warning() {
        let loaded = ok("ui.font.size = 2\nterminal.font.size = 40.5\n");
        assert_eq!(loaded.config.ui_font.size, MIN_FONT_SIZE);
        assert_eq!(loaded.config.terminal_font.size, MAX_FONT_SIZE);
        assert_eq!(loaded.warnings.len(), 2);
        assert_eq!(loaded.warnings[0].line, 1);
        assert!(loaded.warnings[1].message.contains("out of range"));
        let loaded = ok("ui.font.size = 21
");
        assert_eq!(
            loaded.config.ui_font.size, MAX_UI_FONT_SIZE,
            "the window stops at 20"
        );
        assert_eq!(loaded.warnings.len(), 1);
        for edge in ["8", "32", "8.0", "0x10"] {
            let loaded = ok(&format!("terminal.font.size = {edge}"));
            assert!(loaded.warnings.is_empty(), "{edge}");
        }
        assert_eq!(ok("ui.font.size = 0x10").config.ui_font.size, 16.0);
        assert!(ok("ui.font.size = 20").warnings.is_empty());
    }

    #[test]
    fn rf_cfg_02_a_size_that_is_not_a_finite_number_is_invalid() {
        for bad in ["nan", "inf", "-inf", "\"12\"", "true"] {
            let p = err(&format!("terminal.font.size = {bad}"));
            assert_eq!(p.line, 1, "{bad}");
        }
    }

    #[test]
    fn rf_cfg_02_an_empty_family_or_editor_means_the_default() {
        let c = ok("ui.font.family = \"  \"\neditor = \"\"\n").config;
        assert_eq!(c.ui_font.family, None);
        assert_eq!(c.editor, None);
    }

    #[test]
    fn rf_cfg_03_keys_must_be_chords() {
        let p = err("[keys]\n\"pane.close\" = 3\n");
        assert_eq!(p.line, 2);
        let p = err("[keys]\n\"pane.close\" = [\"ctrl-w\", 3]\n");
        assert_eq!((p.line, p.column), (2, 27));
        let p = err("keys = \"ctrl-w\"\n");
        assert!(p.message.contains("keys must be a table"), "{p}");
        // Blank chords are kept for the UI to report; only `[]` is empty.
        let blank = ok("[keys]
\"pane.close\" = [\"\", \" \"]
\"pane.split_down\" = []
");
        assert_eq!(blank.config.keys[0].chords, ["", " "]);
        assert!(blank.config.keys[1].chords.is_empty());
    }

    #[test]
    fn a_file_too_large_is_refused() {
        let big = format!("# {}\n", "x".repeat(MAX_FILE_BYTES));
        assert_eq!(err(&big).line, 1);
    }

    #[test]
    fn every_schema_key_is_handled() {
        for s in crate::SCHEMA {
            let loaded = ok(&format!("{} = {}", s.key, s.default));
            assert!(
                loaded.warnings.is_empty(),
                "{}: {:?}",
                s.key,
                loaded.warnings
            );
        }
    }
}
