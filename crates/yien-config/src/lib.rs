//! YIEN's settings file, `config.toml` (RF-CFG-01, RF-CFG-02, RF-CFG-03).
//!
//! Layer 1: no window, no OS calls, no clock. [`parse`] turns the file's text
//! into a [`Config`] or the [`Problem`] that makes it invalid, with its line
//! and column; [`default_file`] is the commented file written on first run;
//! [`ReloadGate`] decides when a changed file is read again.
//!
//! ```
//! use yien_config::{ThemeSetting, parse};
//!
//! let loaded = parse("theme = \"washi\"\nterminal.font.size = 16\n").unwrap();
//! assert_eq!(loaded.config.theme, ThemeSetting::Washi);
//! assert_eq!(loaded.config.terminal_font.size, 16.0);
//!
//! let bad = parse("theme = \"washi\"\nterminal.font.size = \"big\"\n").unwrap_err();
//! assert_eq!(bad.line, 2);
//! ```

#![forbid(unsafe_code)]

mod command;
mod parse;
mod reload;
mod schema;

pub use command::split_command;
pub use parse::parse;
pub use reload::{Reload, ReloadGate};
pub use schema::{SCHEMA, Setting, default_file};

/// The file's name inside YIEN's data dir.
pub const FILE_NAME: &str = "config.toml";

/// The largest file read; anything longer is refused as invalid (plan §15.1:
/// a tampered file must not stall the app).
pub const MAX_FILE_BYTES: usize = 256 * 1024;

/// The smallest font size kept; smaller values are raised to it.
pub const MIN_FONT_SIZE: f32 = 8.0;

/// The largest terminal font size kept; larger values are lowered to it.
pub const MAX_FONT_SIZE: f32 = 32.0;

/// The largest window text size kept: the title and status bars have fixed
/// heights, and past this their text is clipped.
pub const MAX_UI_FONT_SIZE: f32 = 20.0;

/// The size of both fonts when the file does not set one.
pub const DEFAULT_FONT_SIZE: f32 = 13.0;

/// Which palette the window uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeSetting {
    /// Ink: the dark identity theme.
    #[default]
    Sumi,
    /// Paper: the light identity theme.
    Washi,
    /// Washi while the OS is in light mode, sumi in dark mode.
    System,
}

/// One font role: a family and a size in logical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct FontSetting {
    /// `None` means YIEN's own preference list for the role.
    pub family: Option<String>,
    /// Always within [`MIN_FONT_SIZE`]..=[`MAX_FONT_SIZE`], and at most
    /// [`MAX_UI_FONT_SIZE`] for the window.
    pub size: f32,
}

impl Default for FontSetting {
    fn default() -> Self {
        Self {
            family: None,
            size: DEFAULT_FONT_SIZE,
        }
    }
}

/// One entry of the `[keys]` table: the chords a catalog command gets
/// instead of its default ones (RF-CFG-03).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyOverride {
    /// A command id of the UI's catalog, such as `pane.split_right`. Not
    /// checked here: the catalog lives in the UI.
    pub command: String,
    /// Chords as written, in gpui notation (`ctrl-shift-p`); an empty list
    /// (`[]`) unbinds the command.
    pub chords: Vec<String>,
    /// The line the entry is on, for notices.
    pub line: usize,
}

/// Every setting, with the defaults for what the file leaves out.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    /// `theme`.
    pub theme: ThemeSetting,
    /// `ui.font.family` and `ui.font.size`.
    pub ui_font: FontSetting,
    /// `terminal.font.family` and `terminal.font.size`.
    pub terminal_font: FontSetting,
    /// `editor`: the command "open in editor" runs, `None` for the OS
    /// default (RF-CTX-03).
    pub editor: Option<String>,
    /// `[keys]`, in file order.
    pub keys: Vec<KeyOverride>,
}

/// A place in the file and what is wrong there. Lines and columns start at 1.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}, column {column}: {message}")]
pub struct Problem {
    /// 1-based line.
    pub line: usize,
    /// 1-based column, in characters.
    pub column: usize,
    /// What is wrong, in a sentence without the place.
    pub message: String,
}

impl Problem {
    /// A problem at byte `offset` of `text`.
    pub fn at(text: &str, offset: usize, message: impl Into<String>) -> Self {
        let (line, column) = line_column(text, offset);
        Self {
            line,
            column,
            message: message.into(),
        }
    }
}

/// A file that parsed: its settings and what was accepted with a warning
/// (unknown keys, sizes brought into range).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Loaded {
    /// The settings to apply.
    pub config: Config,
    /// Warnings in file order.
    pub warnings: Vec<Problem>,
}

/// The 1-based line and column (in characters) of a byte offset, clamped to
/// the text and to a character boundary.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let mut end = offset.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let before = &text[..end];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    // A byte order mark is invisible in an editor: it is not a column.
    let column = before[line_start..]
        .chars()
        .filter(|&c| c != '\u{feff}')
        .count()
        + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_become_lines_and_columns() {
        let text = "a = 1\nbé = 2\n";
        assert_eq!(line_column(text, 0), (1, 1));
        assert_eq!(line_column(text, 6), (2, 1));
        // After the two-byte "é", the column counts characters.
        assert_eq!(line_column(text, 9), (2, 3));
        assert_eq!(line_column(text, 8), (2, 2), "inside a character");
        assert_eq!(line_column(text, 999), (3, 1), "past the end");
        assert_eq!(line_column("", 0), (1, 1));
        assert_eq!(
            line_column("\u{feff}a = x", 7),
            (1, 5),
            "the BOM is not counted"
        );
    }

    #[test]
    fn a_problem_reads_with_its_place() {
        let p = Problem::at("x\ny", 2, "bad");
        assert_eq!(p.to_string(), "line 2, column 1: bad");
    }
}
