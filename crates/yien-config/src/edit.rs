//! Setting one key in the file's text, for the settings screen (RF-CFG-05).
//! The file stays the source of truth: the text around the key is kept byte
//! for byte, comments, order and unknown keys included. Every way of placing
//! the key is checked by parsing the result: only the key may change.

use std::ops::Range;

use toml::de::{DeTable, DeValue};

use crate::schema::{self, SCHEMA};
use crate::{Config, Loaded, Problem, ShellSetting, TabGrouping, ThemeSetting, parse};

/// A value as the settings screen shows and writes it.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// A switch.
    Bool(bool),
    /// A size in pixels.
    Number(f32),
    /// A name, a folder or a command line; `""` for the default.
    Text(String),
}

/// Why a key was not set. Nothing is written in any case.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EditError {
    /// The file as it stands does not parse; it is left for the user to fix.
    #[error("config.toml is invalid at {0}; fix it there first")]
    Invalid(Problem),
    /// The key is not one of [`SCHEMA`].
    #[error("unknown setting {0}")]
    UnknownKey(String),
    /// The value is not one the key accepts, in the parser's words.
    #[error("{0}")]
    Rejected(String),
    /// No place in the file takes the key without changing something else.
    #[error("{0} could not be written without changing other settings")]
    Unplaced(String),
}

impl Config {
    /// The value of a [`SCHEMA`] key as the settings screen shows it: the
    /// names the file uses, `""` for an unset family, folder or editor.
    ///
    /// ```
    /// use yien_config::{Config, SettingValue};
    ///
    /// let config = Config::default();
    /// assert_eq!(config.value("theme"), Some(SettingValue::Text("sumi".into())));
    /// assert_eq!(config.value("ui.font.size"), Some(SettingValue::Number(13.0)));
    /// assert_eq!(config.value("colour"), None);
    /// ```
    pub fn value(&self, key: &str) -> Option<SettingValue> {
        let text = |s: &str| Some(SettingValue::Text(s.to_owned()));
        let optional = |s: &Option<String>| text(s.as_deref().unwrap_or(""));
        match key {
            "theme" => text(match self.theme {
                ThemeSetting::Sumi => "sumi",
                ThemeSetting::Washi => "washi",
                ThemeSetting::System => "system",
            }),
            "ui.font.family" => optional(&self.ui_font.family),
            "ui.font.size" => Some(SettingValue::Number(self.ui_font.size)),
            "ui.tabs.group" => text(match self.tab_grouping {
                TabGrouping::Project => "project",
                TabGrouping::State => "state",
            }),
            "ui.tabs.default_dir" => optional(&self.default_tab_dir),
            "terminal.font.family" => optional(&self.terminal_font.family),
            "terminal.font.size" => Some(SettingValue::Number(self.terminal_font.size)),
            "terminal.shell" => ShellSetting::NAMES
                .iter()
                .find(|(s, _)| *s == self.shell)
                .and_then(|(_, name)| text(name)),
            "terminal.shell_integration" => Some(SettingValue::Bool(self.shell_integration)),
            "terminal.osc52_copy" => Some(SettingValue::Bool(self.osc52_copy)),
            "window.close_to_tray" => Some(SettingValue::Bool(self.close_to_tray)),
            "update.check" => Some(SettingValue::Bool(self.update_check)),
            "editor" => optional(&self.editor),
            _ => None,
        }
    }
}

/// Checks `value` for `key` with the file's own parser, as if it were the
/// only line of the file, and returns it as the parser reads it back (a
/// theme lowercased, a family trimmed). A value the parser would warn about,
/// such as a size out of range, is refused.
///
/// ```
/// use yien_config::{SettingValue, check_value};
///
/// let theme = check_value("theme", &SettingValue::Text("Washi".into())).unwrap();
/// assert_eq!(theme, SettingValue::Text("washi".into()));
/// assert!(check_value("ui.font.size", &SettingValue::Number(40.0)).is_err());
/// ```
pub fn check_value(key: &str, value: &SettingValue) -> Result<SettingValue, EditError> {
    if schema::setting(key).is_none() {
        return Err(EditError::UnknownKey(key.to_owned()));
    }
    if let SettingValue::Number(n) = value
        && !n.is_finite()
    {
        return Err(EditError::Rejected(format!(
            "{key} must be a finite number"
        )));
    }
    let alone =
        parse(&format!("{key} = {}\n", repr(value))).map_err(|p| EditError::Rejected(p.message))?;
    if let Some(warning) = alone.warnings.first() {
        return Err(EditError::Rejected(warning.message.clone()));
    }
    alone
        .config
        .value(key)
        .ok_or_else(|| EditError::UnknownKey(key.to_owned()))
}

/// The file's text with `key` set to `value`, everything else as it was.
/// An existing value is replaced where it is, in whatever form the file
/// writes it (`ui.font.size`, `[ui.font]`, `font.size` under `[ui]`, an
/// inline table). A missing key takes, in order: its commented-out line
/// (`# ui.font.size = 13`, as the first-run file has), a line under its
/// table's header, a line before the first table. An invalid file is
/// refused, never written over.
///
/// ```
/// use yien_config::{SettingValue, set_value};
///
/// let text = "# Small text.\nui.font.size = 12 # mine\n";
/// let set = set_value(text, "ui.font.size", &SettingValue::Number(17.0)).unwrap();
/// assert_eq!(set, "# Small text.\nui.font.size = 17 # mine\n");
/// assert!(set_value("theme = 3\n", "theme", &SettingValue::Text("washi".into())).is_err());
/// ```
pub fn set_value(text: &str, key: &str, value: &SettingValue) -> Result<String, EditError> {
    let expected = check_value(key, value)?;
    let old = parse(text).map_err(EditError::Invalid)?;
    let repr = repr(value);
    if let Some(span) = value_span(text, key) {
        let mut new = String::with_capacity(text.len() + repr.len());
        new.push_str(&text[..span.start]);
        new.push_str(&repr);
        new.push_str(&text[span.end..]);
        return if new == text || keeps_the_rest(&old, &new, key, &expected) {
            Ok(new)
        } else {
            Err(EditError::Unplaced(key.to_owned()))
        };
    }
    placements(text, key, &repr)
        .into_iter()
        .find(|new| keeps_the_rest(&old, new, key, &expected))
        .ok_or_else(|| EditError::Unplaced(key.to_owned()))
}

/// The value as TOML: a whole number without a fraction, a string in single
/// quotes when it holds a backslash (a Windows path reads as typed), in
/// double quotes with escapes otherwise.
fn repr(value: &SettingValue) -> String {
    match value {
        SettingValue::Bool(b) => b.to_string(),
        SettingValue::Number(n) if n.fract() == 0.0 && n.abs() < 1e9 => {
            format!("{}", *n as i64)
        }
        SettingValue::Number(n) => format!("{n}"),
        SettingValue::Text(s) => {
            let plain = !s.chars().any(|c| c.is_control() || c == '\'');
            if plain && s.contains('\\') {
                return format!("'{s}'");
            }
            let mut out = String::from('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\t' => out.push_str("\\t"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
    }
}

/// Where the key's value is written, if it is.
fn value_span(text: &str, key: &str) -> Option<Range<usize>> {
    let root = DeTable::parse(text).ok()?;
    let mut table: &DeTable<'_> = root.get_ref();
    let mut parts = key.split('.').peekable();
    while let Some(part) = parts.next() {
        let (_, value) = table.iter().find(|(k, _)| k.get_ref() == part)?;
        if parts.peek().is_none() {
            return Some(value.span());
        }
        match value.get_ref() {
            DeValue::Table(inner) => table = inner,
            _ => return None,
        }
    }
    None
}

/// Whether `new` parses to `old` with only `key` changed, to `expected`.
/// Warnings may move to other lines; those about the key itself may go.
fn keeps_the_rest(old: &Loaded, new: &str, key: &str, expected: &SettingValue) -> bool {
    let Ok(new) = parse(new) else {
        return false;
    };
    let same_settings = SCHEMA.iter().all(|s| {
        let want = if s.key == key {
            Some(expected.clone())
        } else {
            old.config.value(s.key)
        };
        new.config.value(s.key) == want
    });
    let chords = |l: &Loaded| -> Vec<(String, Vec<String>)> {
        l.config
            .keys
            .iter()
            .map(|k| (k.command.clone(), k.chords.clone()))
            .collect()
    };
    let about_key = format!("{key} ");
    let warnings = |l: &Loaded| -> Vec<String> {
        let mut all: Vec<String> = l
            .warnings
            .iter()
            .map(|w| w.message.clone())
            .filter(|m| !m.starts_with(&about_key))
            .collect();
        all.sort();
        all
    };
    same_settings && chords(old) == chords(&new) && warnings(old) == warnings(&new)
}

/// One line of the text: where it starts, its content and its ending.
struct Line<'t> {
    start: usize,
    content: &'t str,
    ending: &'t str,
}

fn lines(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for raw in text.split_inclusive('\n') {
        let content = raw.trim_end_matches(['\n', '\r']);
        out.push(Line {
            start,
            content,
            ending: &raw[content.len()..],
        });
        start += raw.len();
    }
    out
}

/// The texts a missing key could make, best first; the caller keeps the
/// first that parses to the intended settings.
fn placements(text: &str, key: &str, repr: &str) -> Vec<String> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let lines = lines(text);
    let insert = |at: usize, line: &str| -> String {
        let mut new = String::with_capacity(text.len() + line.len() + 2);
        new.push_str(&text[..at]);
        if at > 0 && !text[..at].ends_with('\n') {
            new.push_str(eol);
        }
        new.push_str(line);
        new.push_str(eol);
        new.push_str(&text[at..]);
        new
    };
    let mut out = Vec::new();

    // The commented-out default, uncommented where it stands.
    for line in &lines {
        let body = line.content.trim_start();
        let indent = &line.content[..line.content.len() - body.len()];
        let named = body
            .strip_prefix('#')
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix(key))
            .is_some_and(|after| after.trim_start().starts_with('='));
        if named {
            let end = line.start + line.content.len();
            out.push(format!(
                "{}{indent}{key} = {repr}{}",
                &text[..line.start],
                &text[end..]
            ));
        }
    }

    // Under the header of the key's table, the deepest first.
    let parts: Vec<&str> = key.split('.').collect();
    let mut under: Vec<(usize, String)> = Vec::new();
    for line in &lines {
        let Some(path) = header_path(line.content) else {
            continue;
        };
        if path.len() < parts.len() && path.iter().zip(&parts).all(|(a, b)| a == b) {
            let rest = parts[path.len()..].join(".");
            let after = line.start + line.content.len() + line.ending.len();
            under.push((path.len(), insert(after, &format!("{rest} = {repr}"))));
        }
    }
    under.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
    out.extend(under.into_iter().map(|(_, text)| text));

    // At the top level: before a table and the comments right above it, or
    // at the end.
    let own = format!("{key} = {repr}");
    for (at, line) in lines.iter().enumerate() {
        if !line.content.trim_start().starts_with('[') {
            continue;
        }
        let first = lines[..at]
            .iter()
            .rposition(|l| !l.content.trim_start().starts_with('#'))
            .map_or(0, |p| p + 1);
        out.push(insert(lines[first].start, &own));
    }
    out.push(insert(text.len(), &own));
    out
}

/// The table a `[header]` line opens, as its dotted parts; `None` for any
/// other line, an array of tables included.
fn header_path(line: &str) -> Option<Vec<String>> {
    let body = line.trim_start();
    if !body.starts_with('[') || body.starts_with("[[") {
        return None;
    }
    let parsed = DeTable::parse(line).ok()?;
    let mut table: &DeTable<'_> = parsed.get_ref();
    let mut path = Vec::new();
    loop {
        let mut entries = table.iter();
        let Some((key, value)) = entries.next() else {
            break;
        };
        if entries.next().is_some() {
            return None;
        }
        path.push(key.get_ref().to_string());
        match value.get_ref() {
            DeValue::Table(inner) => table = inner,
            _ => return None,
        }
    }
    (!path.is_empty()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_file;

    fn set(text: &str, key: &str, value: SettingValue) -> String {
        set_value(text, key, &value).unwrap_or_else(|e| panic!("{key}: {e}"))
    }

    fn number(n: f32) -> SettingValue {
        SettingValue::Number(n)
    }

    fn text(s: &str) -> SettingValue {
        SettingValue::Text(s.to_owned())
    }

    /// The lines of `after` that differ from `before`'s, as (index, line).
    fn changed_lines(before: &str, after: &str) -> Vec<(usize, String)> {
        let a: Vec<&str> = before.lines().collect();
        let b: Vec<&str> = after.lines().collect();
        assert_eq!(a.len(), b.len(), "a line was added or removed:\n{after}");
        a.iter()
            .zip(&b)
            .enumerate()
            .filter(|(_, (x, y))| x != y)
            .map(|(n, (_, y))| (n, (*y).to_owned()))
            .collect()
    }

    #[test]
    fn rf_cfg_05_an_existing_value_is_replaced_in_place_with_its_comments() {
        let before = "# My settings.\n\n# Bigger text.\nui.font.size = 12 # was 13\n\
                      colour = \"red\"\n\n[keys]\n\"pane.close\" = []\n";
        let after = set(before, "ui.font.size", number(17.0));
        assert_eq!(after, before.replace("= 12 #", "= 17 #"));
    }

    #[test]
    fn rf_cfg_05_the_first_run_file_has_its_commented_line_uncommented() {
        let before = default_file(&[("pane.close", "pane: close")]);
        let after = set(&before, "ui.font.size", number(17.0));
        let changed = changed_lines(&before, &after);
        assert_eq!(changed.len(), 1, "{changed:?}");
        let (at, line) = &changed[0];
        assert_eq!(line, "ui.font.size = 17");
        let above = before.lines().nth(at - 1).unwrap();
        assert_eq!(after.lines().nth(at - 1).unwrap(), above);
        assert!(above.starts_with("# The interface size"), "{above}");
        assert_eq!(parse(&after).unwrap().config.ui_font.size, 17.0);
    }

    #[test]
    fn rf_cfg_05_every_key_can_be_set_in_the_first_run_file() {
        let file = default_file(&[]);
        for (key, value) in [
            ("theme", text("washi")),
            ("ui.font.family", text("Segoe UI")),
            ("ui.font.size", number(15.0)),
            ("ui.tabs.group", text("state")),
            ("ui.tabs.default_dir", text("D:\\Code")),
            ("terminal.font.family", text("Consolas")),
            ("terminal.font.size", number(20.5)),
            ("terminal.shell", text("git-bash")),
            ("terminal.shell_integration", SettingValue::Bool(false)),
            ("terminal.osc52_copy", SettingValue::Bool(false)),
            ("window.close_to_tray", SettingValue::Bool(false)),
            ("update.check", SettingValue::Bool(false)),
            ("editor", text("code --wait")),
        ] {
            let after = set(&file, key, value.clone());
            assert_eq!(changed_lines(&file, &after).len(), 1, "{key}");
            assert_eq!(
                parse(&after).unwrap().config.value(key),
                Some(value),
                "{key}"
            );
        }
    }

    #[test]
    fn rf_cfg_05_every_schema_key_has_a_value() {
        for s in SCHEMA {
            let default = Config::default().value(s.key);
            assert!(default.is_some(), "{}", s.key);
            let written = parse(&format!("{} = {}", s.key, s.default)).unwrap();
            assert_eq!(written.config.value(s.key), default, "{}", s.key);
        }
    }

    #[test]
    fn rf_cfg_05_the_nested_forms_are_edited_where_they_are() {
        for before in [
            "ui.font.size = 13\n",
            "[ui]\nfont.size = 13\n",
            "[ui.font]\nsize = 13\nfamily = \"Segoe UI\"\n",
            "ui = { font = { size = 13 } }\n",
            "ui.font = { size = 13 }\n",
        ] {
            let after = set(before, "ui.font.size", number(16.0));
            assert_eq!(after, before.replace("13", "16"), "{before}");
        }
    }

    #[test]
    fn rf_cfg_05_a_missing_key_goes_under_its_table_header() {
        let before = "theme = \"washi\"\n\n[ui.font]\nfamily = \"Segoe UI\"\n\n[keys]\n";
        let after = set(before, "ui.font.size", number(15.0));
        assert_eq!(
            after,
            "theme = \"washi\"\n\n[ui.font]\nsize = 15\nfamily = \"Segoe UI\"\n\n[keys]\n"
        );
        let before = "[ui]\ntabs.group = \"state\"\n";
        let after = set(before, "ui.font.size", number(15.0));
        assert_eq!(after, "[ui]\nfont.size = 15\ntabs.group = \"state\"\n");
    }

    #[test]
    fn rf_cfg_05_a_missing_key_goes_before_the_first_table_and_its_comment() {
        let before = "theme = \"washi\"\n\n# My keys.\n[keys]\n\"pane.close\" = []\n";
        let after = set(before, "terminal.shell", text("cmd"));
        assert_eq!(
            after,
            "theme = \"washi\"\n\nterminal.shell = \"cmd\"\n# My keys.\n[keys]\n\"pane.close\" = []\n"
        );
    }

    #[test]
    fn rf_cfg_05_a_missing_key_is_added_at_the_end_of_a_file_without_tables() {
        assert_eq!(
            set("", "update.check", SettingValue::Bool(false)),
            "update.check = false\n"
        );
        assert_eq!(
            set("theme = \"washi\"", "editor", text("code")),
            "theme = \"washi\"\neditor = \"code\"\n"
        );
    }

    #[test]
    fn rf_cfg_05_a_table_of_another_key_is_never_used() {
        // `[ui.tabs]` is not a prefix of ui.font.size; the key goes on top.
        let before = "[ui.tabs]\ngroup = \"state\"\n";
        let after = set(before, "ui.font.size", number(14.0));
        assert_eq!(after, "ui.font.size = 14\n[ui.tabs]\ngroup = \"state\"\n");
    }

    #[test]
    fn rf_cfg_05_lines_inside_a_string_are_not_taken_for_places() {
        let before = "notes = \"\"\"\n# ui.font.size = 13\n[ui.font]\n\"\"\"\n";
        let after = set(before, "ui.font.size", number(14.0));
        assert!(after.starts_with(before), "{after}");
        assert!(after.ends_with("ui.font.size = 14\n"), "{after}");
        let loaded = parse(&after).unwrap();
        assert_eq!(loaded.config.ui_font.size, 14.0);
    }

    #[test]
    fn rf_cfg_05_crlf_files_keep_their_line_endings() {
        let before = "theme = \"washi\"\r\n[keys]\r\n";
        let after = set(before, "editor", text("code"));
        assert_eq!(
            after,
            "theme = \"washi\"\r\neditor = \"code\"\r\n[keys]\r\n"
        );
        let commented = "# editor = \"\"\r\ntheme = \"sumi\"\r\n";
        assert_eq!(
            set(commented, "editor", text("vim")),
            "editor = \"vim\"\r\ntheme = \"sumi\"\r\n"
        );
    }

    #[test]
    fn rf_cfg_05_an_invalid_file_is_refused() {
        let bad = "theme = \"washi\"\nui.font.size = \"big\"\n";
        match set_value(bad, "theme", &text("sumi")) {
            Err(EditError::Invalid(p)) => assert_eq!(p.line, 2),
            other => panic!("{other:?}"),
        }
        let err = set_value("[keys\n", "theme", &text("sumi")).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("config.toml is invalid at line 1"),
            "{err}"
        );
    }

    #[test]
    fn rf_cfg_05_values_are_checked_by_the_files_parser() {
        let refused = |key: &str, value: SettingValue| match set_value("", key, &value) {
            Err(EditError::Rejected(why)) => why,
            other => panic!("{key}: {other:?}"),
        };
        assert!(refused("theme", text("blue")).contains("not \"blue\""));
        assert!(refused("ui.font.size", number(25.0)).contains("out of range"));
        assert!(refused("terminal.font.size", number(4.0)).contains("out of range"));
        assert!(refused("theme", SettingValue::Bool(true)).contains("must be a string"));
        assert!(refused("update.check", text("yes")).contains("true or false"));
        assert!(refused("terminal.shell", text("wsl")).contains("must be one of"));
        assert!(refused("ui.font.size", number(f32::NAN)).contains("finite"));
        assert_eq!(
            set_value("", "colour", &text("red")),
            Err(EditError::UnknownKey("colour".into()))
        );
        assert_eq!(
            check_value("terminal.shell", &text(" Git-Bash ")),
            Ok(text("git-bash"))
        );
        assert_eq!(check_value("ui.font.size", &number(20.0)), Ok(number(20.0)));
    }

    #[test]
    fn rf_cfg_05_strings_are_written_so_they_read_back_the_same() {
        for value in [
            "D:\\Code\\my folder",
            "C:\\it's here",
            "code --title \"x\"",
            "名前 ~/work",
            "tab\there",
        ] {
            let after = set("", "ui.tabs.default_dir", text(value));
            let read = parse(&after).unwrap().config.default_tab_dir;
            assert_eq!(read.as_deref(), Some(value), "{after}");
        }
        assert_eq!(
            set("", "ui.tabs.default_dir", text("D:\\Code")),
            "ui.tabs.default_dir = 'D:\\Code'\n"
        );
    }

    #[test]
    fn rf_cfg_05_setting_the_value_already_written_changes_nothing() {
        let before = "# keep\nui.font.size = 13\n";
        assert_eq!(set(before, "ui.font.size", number(13.0)), before);
    }

    #[test]
    fn rf_cfg_05_unknown_keys_and_out_of_range_warnings_are_kept_apart() {
        // The old value warned about its range; the new one does not. The
        // unknown key keeps its warning.
        let before = "colour = 1\nui.font.size = 30\n";
        let after = set(before, "ui.font.size", number(18.0));
        assert_eq!(after, "colour = 1\nui.font.size = 18\n");
        let loaded = parse(&after).unwrap();
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn rf_cfg_05_a_fractional_size_keeps_its_fraction() {
        assert_eq!(
            set("", "terminal.font.size", number(14.5)),
            "terminal.font.size = 14.5\n"
        );
    }

    #[test]
    fn headers_name_their_tables() {
        assert_eq!(
            header_path("[ui.font]"),
            Some(vec!["ui".into(), "font".into()])
        );
        assert_eq!(
            header_path("  [ \"ui\" . font ]  # mine"),
            Some(vec!["ui".into(), "font".into()])
        );
        assert_eq!(header_path("[[ui]]"), None);
        assert_eq!(header_path("ui.font.size = 3"), None);
        assert_eq!(header_path("[ui"), None);
    }
}
