//! The settings screen's model (RF-CFG-05): which rows it has, which the
//! find field leaves, where the keyboard is and what a key does to a value.
//! It shows the file's values; a change goes to the file, and the screen
//! shows it once the file is read back (RF-CFG-01). Drawing is in
//! `crate::shell::settings_screen`.

use yien_config::{Config, MAX_FONT_SIZE, MAX_UI_FONT_SIZE, MIN_FONT_SIZE, SettingValue};

use crate::catalog::CATALOG;

/// The screen's sections, in the order the list shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Appearance,
    Tabs,
    Terminal,
    General,
    /// The catalog's commands and their keys, read-only.
    Keys,
}

impl Section {
    pub(crate) const ALL: [Self; 5] = [
        Self::Appearance,
        Self::Tabs,
        Self::Terminal,
        Self::General,
        Self::Keys,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Tabs => "Tabs",
            Self::Terminal => "Terminal",
            Self::General => "General",
            Self::Keys => "Keys",
        }
    }

    /// The section `by` places away, wrapping around.
    fn moved(self, by: isize) -> Self {
        let at = Self::ALL.iter().position(|s| *s == self).unwrap_or(0) as isize;
        let len = Self::ALL.len() as isize;
        Self::ALL[(at + by).rem_euclid(len) as usize]
    }
}

/// One value a choice control offers: what it shows and what the file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) label: &'static str,
    pub(crate) value: &'static str,
}

/// How a row's value is changed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Control {
    /// A few choices side by side.
    Segmented(&'static [Choice]),
    /// More choices, listed under the box when it opens.
    Select(&'static [Choice]),
    /// A size in whole pixels between two bounds.
    Stepper { min: f32, max: f32 },
    /// Free text; `empty` is what an empty field means.
    Text { empty: &'static str, mono: bool },
    /// On or off.
    Switch,
}

/// One setting on the screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Row {
    /// The file's key, shown beside the label.
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    /// One short line.
    pub(crate) doc: &'static str,
    pub(crate) section: Section,
    pub(crate) control: Control,
}

const THEMES: &[Choice] = &[
    Choice {
        label: "Sumi",
        value: "sumi",
    },
    Choice {
        label: "Washi",
        value: "washi",
    },
    Choice {
        label: "System",
        value: "system",
    },
];

const GROUPINGS: &[Choice] = &[
    Choice {
        label: "Project",
        value: "project",
    },
    Choice {
        label: "State",
        value: "state",
    },
];

const SHELLS: &[Choice] = &[
    Choice {
        label: "Auto",
        value: "auto",
    },
    Choice {
        label: "Command Prompt",
        value: "cmd",
    },
    Choice {
        label: "Windows PowerShell",
        value: "powershell",
    },
    Choice {
        label: "PowerShell 7",
        value: "pwsh",
    },
    Choice {
        label: "Git Bash",
        value: "git-bash",
    },
];

const OS_DEFAULT: &str = if cfg!(windows) {
    "Windows default"
} else {
    "System default"
};

/// Every setting of the file, by section, in the order drawn.
pub(crate) const ROWS: &[Row] = &[
    Row {
        key: "theme",
        label: "Theme",
        doc: "Sumi, washi, or follow Windows.",
        section: Section::Appearance,
        control: Control::Segmented(THEMES),
    },
    Row {
        key: "ui.font.size",
        label: "Interface size",
        doc: "Text, rows, widths and icons; 8 to 20.",
        section: Section::Appearance,
        control: Control::Stepper {
            min: MIN_FONT_SIZE,
            max: MAX_UI_FONT_SIZE,
        },
    },
    Row {
        key: "ui.font.family",
        label: "Interface font",
        doc: "Empty: Zen Kaku Gothic New, Yu Gothic UI, Segoe UI.",
        section: Section::Appearance,
        control: Control::Text {
            empty: "Default",
            mono: false,
        },
    },
    Row {
        key: "terminal.font.family",
        label: "Terminal font",
        doc: "Monospaced. Empty: M PLUS 1 Code, Cascadia Mono, Consolas.",
        section: Section::Appearance,
        control: Control::Text {
            empty: "Default",
            mono: false,
        },
    },
    Row {
        key: "terminal.font.size",
        label: "Terminal text size",
        doc: "8 to 32. Ctrl+= and Ctrl+- zoom from it.",
        section: Section::Appearance,
        control: Control::Stepper {
            min: MIN_FONT_SIZE,
            max: MAX_FONT_SIZE,
        },
    },
    Row {
        key: "ui.tabs.group",
        label: "List tabs by",
        doc: "Under their projects, or by agent state.",
        section: Section::Tabs,
        control: Control::Segmented(GROUPINGS),
    },
    Row {
        key: "ui.tabs.default_dir",
        label: "New tab folder",
        doc: "Where the new tab button opens a shell.",
        section: Section::Tabs,
        control: Control::Text {
            empty: "Home folder",
            mono: true,
        },
    },
    Row {
        key: "terminal.shell",
        label: "Shell",
        doc: "For panes started after a change.",
        section: Section::Terminal,
        control: Control::Select(SHELLS),
    },
    Row {
        key: "terminal.shell_integration",
        label: "Shell integration",
        doc: "Panes reopen in the folder you left them in.",
        section: Section::Terminal,
        control: Control::Switch,
    },
    Row {
        key: "terminal.osc52_copy",
        label: "Programs may copy",
        doc: "OSC 52 writes to the clipboard, as Claude Code and vim do.",
        section: Section::Terminal,
        control: Control::Switch,
    },
    Row {
        key: "editor",
        label: "Editor",
        doc: "The command that opens files, such as code.",
        section: Section::General,
        control: Control::Text {
            empty: OS_DEFAULT,
            mono: false,
        },
    },
    Row {
        key: "window.close_to_tray",
        label: "Close to tray",
        doc: "Closing the window keeps YIEN and its agents running.",
        section: Section::General,
        control: Control::Switch,
    },
    Row {
        key: "update.check",
        label: "Check for updates",
        doc: "Once a day; nothing is installed until you ask.",
        section: Section::General,
        control: Control::Switch,
    },
];

/// The row of `key`.
pub(crate) fn row(key: &str) -> Option<&'static Row> {
    ROWS.iter().find(|r| r.key == key)
}

/// A place the keyboard can be in the body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Stop {
    /// A setting.
    Row(&'static Row),
    /// The Keys section's "Edit in config.toml".
    EditKeys,
    /// A catalog command in the Keys section, by its index.
    Command(usize),
}

/// What a key or a click asks of the shell.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Act {
    /// Nothing to do.
    None,
    /// Write this value of this key.
    Write(&'static str, SettingValue),
    /// Give the keyboard to this row's text field.
    EditText(&'static str),
    /// Open the settings file.
    OpenFile,
}

/// The screen while it is open.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Screen {
    section: Section,
    query: String,
    /// The stop under the keyboard; `None` while the find field has it.
    cursor: Option<usize>,
    /// The select whose list is open, and the choice under the keyboard.
    open_select: Option<(&'static str, usize)>,
    /// Values written and not yet read back, oldest first; the last of a
    /// key shows in place of the file's.
    pending: Vec<Pending>,
}

/// A value sent to the file and not yet read back.
#[derive(Debug, Clone, PartialEq)]
struct Pending {
    key: &'static str,
    value: SettingValue,
    /// What the file held for the key before the first of its writes.
    before: Option<SettingValue>,
}

impl Default for Screen {
    fn default() -> Self {
        Self {
            section: Section::Appearance,
            query: String::new(),
            cursor: None,
            open_select: None,
            pending: Vec::new(),
        }
    }
}

impl Screen {
    pub(crate) fn section(&self) -> Section {
        self.section
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// Whether the body lists find results rather than a section.
    pub(crate) fn finding(&self) -> bool {
        !self.query.trim().is_empty()
    }

    pub(crate) fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// The stop under the keyboard, if the body has it.
    pub(crate) fn cursor_stop(&self) -> Option<Stop> {
        self.stops().get(self.cursor?).copied()
    }

    pub(crate) fn open_select(&self) -> Option<(&'static str, usize)> {
        self.open_select
    }

    /// The places the keyboard goes in the body, top to bottom: the rows
    /// that match the find field, or the section's.
    pub(crate) fn stops(&self) -> Vec<Stop> {
        if self.finding() {
            return ROWS
                .iter()
                .filter(|r| matches(&self.query, r))
                .map(Stop::Row)
                .collect();
        }
        match self.section {
            Section::Keys => std::iter::once(Stop::EditKeys)
                .chain((0..CATALOG.len()).map(Stop::Command))
                .collect(),
            section => ROWS
                .iter()
                .filter(|r| r.section == section)
                .map(Stop::Row)
                .collect(),
        }
    }

    /// Shows `section`; the find field is cleared, and the keyboard stays
    /// in the body when it was there.
    pub(crate) fn show_section(&mut self, section: Section) {
        self.section = section;
        self.query.clear();
        self.open_select = None;
        self.cursor = self.cursor.map(|_| 0).filter(|_| !self.stops().is_empty());
    }

    /// Ctrl+Tab and Ctrl+Shift+Tab, Page Down and Page Up.
    pub(crate) fn next_section(&mut self, forward: bool) {
        self.show_section(self.section.moved(if forward { 1 } else { -1 }));
    }

    /// The find field's text changed; the keyboard stays in it.
    pub(crate) fn set_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.cursor = None;
        self.open_select = None;
    }

    /// Down: from the find field to the first stop, then one stop down.
    /// In an open select, the next choice.
    pub(crate) fn down(&mut self) {
        if let Some((key, at)) = self.open_select {
            let len = choices(key).len();
            self.open_select = Some((key, (at + 1).min(len.saturating_sub(1))));
            return;
        }
        let len = self.stops().len();
        if len == 0 {
            return;
        }
        self.cursor = Some(self.cursor.map_or(0, |at| (at + 1).min(len - 1)));
    }

    /// Up: one stop up, and from the first stop back to the find field.
    pub(crate) fn up(&mut self) {
        if let Some((key, at)) = self.open_select {
            self.open_select = Some((key, at.saturating_sub(1)));
            return;
        }
        self.cursor = match self.cursor {
            Some(0) | None => None,
            Some(at) => Some(at - 1),
        };
    }

    /// Puts the keyboard on the stop of `key`, as a click on its row does.
    pub(crate) fn point_at(&mut self, key: &str) {
        if let Some(at) = self
            .stops()
            .iter()
            .position(|s| matches!(s, Stop::Row(r) if r.key == key))
        {
            self.cursor = Some(at);
        }
    }

    /// Puts the keyboard back in the find field.
    #[cfg(test)]
    pub(crate) fn back_to_find(&mut self) {
        self.cursor = None;
        self.open_select = None;
    }

    /// Left and Right on the row under the keyboard: the previous or next
    /// choice, one pixel less or more. Choices stop at the ends.
    pub(crate) fn step(&self, config: &Config, forward: bool) -> Act {
        let Some(Stop::Row(row)) = self.cursor_stop() else {
            return Act::None;
        };
        if self.open_select.is_some() {
            return Act::None;
        }
        match row.control {
            Control::Segmented(list) | Control::Select(list) => {
                let at = choice_index(list, &self.shown(row.key, config));
                let next = if forward {
                    (at + 1).min(list.len() - 1)
                } else {
                    at.saturating_sub(1)
                };
                write_if_changed(row.key, list[next].value, at != next)
            }
            Control::Stepper { min, max } => match self.shown(row.key, config) {
                SettingValue::Number(now) => stepped(row.key, now, min, max, forward),
                _ => Act::None,
            },
            Control::Text { .. } | Control::Switch => Act::None,
        }
    }

    /// Enter or Space on the stop under the keyboard: a switch flips, a
    /// choice moves to the next (wrapping), a select opens or takes the
    /// choice under the keyboard, a text field takes the keyboard.
    pub(crate) fn activate(&mut self, config: &Config) -> Act {
        let Some(stop) = self.cursor_stop() else {
            return Act::None;
        };
        let row = match stop {
            Stop::EditKeys => return Act::OpenFile,
            Stop::Command(_) => return Act::None,
            Stop::Row(row) => row,
        };
        match row.control {
            Control::Switch => match self.shown(row.key, config) {
                SettingValue::Bool(on) => Act::Write(row.key, SettingValue::Bool(!on)),
                _ => Act::None,
            },
            Control::Segmented(list) => {
                let at = choice_index(list, &self.shown(row.key, config));
                let next = (at + 1) % list.len();
                write_if_changed(row.key, list[next].value, at != next)
            }
            Control::Select(list) => match self.open_select.take() {
                Some((key, at)) if key == row.key => {
                    let now = choice_index(list, &self.shown(row.key, config));
                    let at = at.min(list.len() - 1);
                    write_if_changed(row.key, list[at].value, at != now)
                }
                _ => {
                    let now = choice_index(list, &self.shown(row.key, config));
                    self.open_select = Some((row.key, now));
                    Act::None
                }
            },
            Control::Stepper { .. } => Act::None,
            Control::Text { .. } => Act::EditText(row.key),
        }
    }

    /// Closes an open select list; `false` when none was open.
    pub(crate) fn close_select(&mut self) -> bool {
        self.open_select.take().is_some()
    }

    /// What a click on a choice of `key` writes.
    pub(crate) fn pick(&mut self, key: &'static str, value: &'static str, config: &Config) -> Act {
        self.open_select = None;
        self.point_at(key);
        let changed = self.shown(key, config) != SettingValue::Text(value.to_owned());
        write_if_changed(key, value, changed)
    }

    /// A click on a stepper's minus or plus.
    pub(crate) fn click_step(&mut self, key: &'static str, forward: bool, config: &Config) -> Act {
        self.point_at(key);
        let Some(Row {
            control: Control::Stepper { min, max },
            ..
        }) = row(key)
        else {
            return Act::None;
        };
        match self.shown(key, config) {
            SettingValue::Number(now) => stepped(key, now, *min, *max, forward),
            _ => Act::None,
        }
    }

    /// A click on a switch.
    pub(crate) fn click_switch(&mut self, key: &'static str, config: &Config) -> Act {
        self.point_at(key);
        match self.shown(key, config) {
            SettingValue::Bool(on) => Act::Write(key, SettingValue::Bool(!on)),
            _ => Act::None,
        }
    }

    /// A click on a select's box: opens its list, or closes it.
    pub(crate) fn click_select(&mut self, key: &'static str, config: &Config) {
        self.point_at(key);
        if self.open_select.is_some_and(|(open, _)| open == key) {
            self.open_select = None;
            return;
        }
        if let Some(Row {
            control: Control::Select(list),
            ..
        }) = row(key)
        {
            self.open_select = Some((key, choice_index(list, &self.shown(key, config))));
        }
    }

    /// The value the screen shows for `key`: the one written last until the
    /// file is read back, else the file's.
    pub(crate) fn shown(&self, key: &str, config: &Config) -> SettingValue {
        self.pending
            .iter()
            .rev()
            .find(|p| p.key == key)
            .map(|p| p.value.clone())
            .or_else(|| config.value(key))
            .unwrap_or(SettingValue::Text(String::new()))
    }

    /// A value was sent to the file, which held `config`'s.
    pub(crate) fn written(&mut self, key: &'static str, value: SettingValue, config: &Config) {
        let before = match self.pending.iter().find(|p| p.key == key) {
            Some(first) => first.before.clone(),
            None => config.value(key),
        };
        self.pending.push(Pending { key, value, before });
    }

    /// The write of `key` failed: the file's value shows again.
    pub(crate) fn write_failed(&mut self, key: &str) {
        self.pending.retain(|p| p.key != key);
    }

    /// The file was read and holds `config`. A key whose value is one it
    /// was sent drops that write and the ones before it; a key still at
    /// its old value keeps waiting, as its writes are queued; any other
    /// value was written by someone else, and wins.
    pub(crate) fn file_read(&mut self, config: &Config) {
        let mut keys: Vec<&'static str> = self.pending.iter().map(|p| p.key).collect();
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            let now = config.value(key);
            let mine: Vec<usize> = (0..self.pending.len())
                .filter(|&at| self.pending[at].key == key)
                .collect();
            let confirmed = mine
                .iter()
                .position(|&at| Some(&self.pending[at].value) == now.as_ref());
            let drop: Vec<usize> = match confirmed {
                Some(upto) => mine[..=upto].to_vec(),
                None if mine
                    .first()
                    .is_some_and(|&at| self.pending[at].before == now) =>
                {
                    Vec::new()
                }
                None => mine,
            };
            let mut at = 0;
            self.pending.retain(|_| {
                let keep = !drop.contains(&at);
                at += 1;
                keep
            });
        }
    }
}

/// Whether `row` matches every word of the find field, in its label, key,
/// line or section, ignoring case.
pub(crate) fn matches(query: &str, row: &Row) -> bool {
    let hay = format!(
        "{} {} {} {}",
        row.label,
        row.key,
        row.doc,
        row.section.name()
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|word| hay.contains(&word.to_lowercase()))
}

/// The choices of a segmented or select row.
pub(crate) fn choices(key: &str) -> &'static [Choice] {
    match row(key).map(|r| r.control) {
        Some(Control::Segmented(list) | Control::Select(list)) => list,
        _ => &[],
    }
}

/// Where `value` is in `list`; the first choice for anything else.
pub(crate) fn choice_index(list: &[Choice], value: &SettingValue) -> usize {
    match value {
        SettingValue::Text(v) => list.iter().position(|c| c.value == v).unwrap_or(0),
        _ => 0,
    }
}

/// A size as the stepper shows it: `13 px`, `14.5 px`.
pub(crate) fn size_label(value: &SettingValue) -> String {
    match value {
        SettingValue::Number(n) if n.fract() == 0.0 => format!("{n:.0} px"),
        SettingValue::Number(n) => format!("{n} px"),
        _ => String::new(),
    }
}

/// The next whole pixel up or down from `now`, kept between the bounds.
fn stepped(key: &'static str, now: f32, min: f32, max: f32, forward: bool) -> Act {
    let next = if forward {
        now.floor() + 1.0
    } else {
        now.ceil() - 1.0
    }
    .clamp(min, max);
    if next == now {
        Act::None
    } else {
        Act::Write(key, SettingValue::Number(next))
    }
}

fn write_if_changed(key: &'static str, value: &'static str, changed: bool) -> Act {
    if changed {
        Act::Write(key, SettingValue::Text(value.to_owned()))
    } else {
        Act::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        yien_config::parse(text).unwrap().config
    }

    fn keys(stops: &[Stop]) -> Vec<&'static str> {
        stops
            .iter()
            .filter_map(|s| match s {
                Stop::Row(r) => Some(r.key),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rf_cfg_05_every_key_of_the_file_is_on_the_screen_once() {
        for setting in yien_config::SCHEMA {
            let rows = ROWS.iter().filter(|r| r.key == setting.key).count();
            assert_eq!(rows, 1, "{}", setting.key);
        }
        assert_eq!(ROWS.len(), yien_config::SCHEMA.len());
    }

    #[test]
    fn rf_cfg_05_every_choice_is_a_value_the_file_accepts() {
        for row in ROWS {
            if let Control::Segmented(list) | Control::Select(list) = row.control {
                for choice in list {
                    let value = SettingValue::Text(choice.value.to_owned());
                    assert_eq!(
                        yien_config::check_value(row.key, &value),
                        Ok(value),
                        "{}",
                        row.key
                    );
                }
            }
        }
        // Every shell the file knows has a choice.
        for (_, name) in yien_config::ShellSetting::NAMES {
            assert!(SHELLS.iter().any(|c| c.value == name), "{name}");
        }
    }

    #[test]
    fn rf_cfg_05_every_control_fits_its_value() {
        let defaults = Config::default();
        for row in ROWS {
            let value = defaults.value(row.key).unwrap();
            let fits = matches!(
                (row.control, &value),
                (Control::Switch, SettingValue::Bool(_))
                    | (Control::Stepper { .. }, SettingValue::Number(_))
                    | (
                        Control::Segmented(_) | Control::Select(_) | Control::Text { .. },
                        SettingValue::Text(_)
                    )
            );
            assert!(fits, "{}", row.key);
        }
    }

    #[test]
    fn rf_cfg_05_sections_list_their_rows_in_order() {
        let mut screen = Screen::default();
        assert_eq!(
            keys(&screen.stops()),
            [
                "theme",
                "ui.font.size",
                "ui.font.family",
                "terminal.font.family",
                "terminal.font.size"
            ]
        );
        screen.show_section(Section::Terminal);
        assert_eq!(
            keys(&screen.stops()),
            [
                "terminal.shell",
                "terminal.shell_integration",
                "terminal.osc52_copy"
            ]
        );
        screen.show_section(Section::Keys);
        let stops = screen.stops();
        assert_eq!(stops[0], Stop::EditKeys);
        assert_eq!(stops.len(), CATALOG.len() + 1);
    }

    #[test]
    fn rf_cfg_05_find_lists_matching_rows_of_every_section() {
        let mut screen = Screen::default();
        screen.set_query("size");
        assert_eq!(
            keys(&screen.stops()),
            ["ui.font.size", "terminal.font.size"]
        );
        screen.set_query("OSC");
        assert_eq!(keys(&screen.stops()), ["terminal.osc52_copy"]);
        screen.set_query("tray close");
        assert_eq!(keys(&screen.stops()), ["window.close_to_tray"]);
        screen.set_query("terminal font");
        assert_eq!(
            keys(&screen.stops()),
            ["terminal.font.family", "terminal.font.size"]
        );
        screen.set_query("ui.tabs");
        assert_eq!(
            keys(&screen.stops()),
            ["ui.tabs.group", "ui.tabs.default_dir"]
        );
        screen.set_query("nothing like this");
        assert!(screen.stops().is_empty());
        screen.set_query("   ");
        assert!(!screen.finding(), "blank is no query");
    }

    #[test]
    fn rf_cfg_05_the_arrows_go_from_the_find_field_through_the_rows_and_back() {
        let mut screen = Screen::default();
        assert_eq!(screen.cursor(), None, "the find field has the keyboard");
        screen.up();
        assert_eq!(screen.cursor(), None);
        screen.down();
        assert_eq!(screen.cursor(), Some(0));
        for _ in 0..10 {
            screen.down();
        }
        assert_eq!(screen.cursor(), Some(4), "stops at the last row");
        screen.up();
        assert_eq!(screen.cursor(), Some(3));
        for _ in 0..4 {
            screen.up();
        }
        assert_eq!(screen.cursor(), None, "back in the find field");
        screen.set_query("zzz");
        screen.down();
        assert_eq!(screen.cursor(), None, "nothing to go to");
    }

    #[test]
    fn rf_cfg_05_sections_cycle_and_keep_the_keyboard_in_the_body() {
        let mut screen = Screen::default();
        screen.down();
        screen.down();
        screen.next_section(true);
        assert_eq!(screen.section(), Section::Tabs);
        assert_eq!(screen.cursor(), Some(0));
        screen.next_section(false);
        screen.next_section(false);
        assert_eq!(screen.section(), Section::Keys, "wraps around");
        screen.back_to_find();
        screen.set_query("size");
        screen.next_section(true);
        assert_eq!(screen.section(), Section::Appearance);
        assert_eq!(screen.query(), "", "a section clears the find field");
        assert_eq!(screen.cursor(), None);
    }

    #[test]
    fn rf_cfg_05_left_and_right_change_choices_and_sizes() {
        let config = config("theme = \"washi\"\nui.font.size = 20\n");
        let mut screen = Screen::default();
        screen.down();
        assert_eq!(
            screen.step(&config, true),
            Act::Write("theme", SettingValue::Text("system".into()))
        );
        assert_eq!(
            screen.step(&config, false),
            Act::Write("theme", SettingValue::Text("sumi".into()))
        );
        let at_end = Config {
            theme: yien_config::ThemeSetting::System,
            ..config.clone()
        };
        assert_eq!(screen.step(&at_end, true), Act::None, "no wrap");
        screen.down();
        assert_eq!(screen.step(&config, true), Act::None, "20 is the most");
        assert_eq!(
            screen.step(&config, false),
            Act::Write("ui.font.size", SettingValue::Number(19.0))
        );
        screen.down();
        assert_eq!(screen.step(&config, true), Act::None, "a text field");
    }

    #[test]
    fn rf_cfg_05_a_fractional_size_steps_to_whole_pixels() {
        let config = config("terminal.font.size = 14.5\n");
        let mut screen = Screen::default();
        assert_eq!(
            screen.click_step("terminal.font.size", true, &config),
            Act::Write("terminal.font.size", SettingValue::Number(15.0))
        );
        assert_eq!(
            screen.click_step("terminal.font.size", false, &config),
            Act::Write("terminal.font.size", SettingValue::Number(14.0))
        );
        assert_eq!(
            screen
                .cursor_stop()
                .map(|s| matches!(s, Stop::Row(r) if r.key == "terminal.font.size")),
            Some(true)
        );
    }

    #[test]
    fn rf_cfg_05_enter_flips_switches_and_opens_text_fields() {
        let config = Config::default();
        let mut screen = Screen::default();
        screen.show_section(Section::General);
        screen.down();
        assert_eq!(screen.activate(&config), Act::EditText("editor"));
        screen.down();
        assert_eq!(
            screen.activate(&config),
            Act::Write("window.close_to_tray", SettingValue::Bool(false))
        );
        screen.back_to_find();
        assert_eq!(screen.activate(&config), Act::None, "in the find field");
        screen.show_section(Section::Keys);
        screen.down();
        assert_eq!(screen.activate(&config), Act::OpenFile);
        screen.down();
        assert_eq!(
            screen.activate(&config),
            Act::None,
            "commands are read-only"
        );
    }

    #[test]
    fn rf_cfg_05_a_select_opens_moves_and_picks() {
        let config = Config::default();
        let mut screen = Screen::default();
        screen.show_section(Section::Terminal);
        screen.down();
        assert_eq!(screen.activate(&config), Act::None);
        assert_eq!(screen.open_select(), Some(("terminal.shell", 0)));
        screen.down();
        screen.down();
        assert_eq!(screen.cursor(), Some(0), "the list has the arrows");
        assert_eq!(
            screen.activate(&config),
            Act::Write("terminal.shell", SettingValue::Text("powershell".into()))
        );
        assert_eq!(screen.open_select(), None);
        screen.activate(&config);
        assert!(screen.close_select());
        assert!(!screen.close_select());
        screen.click_select("terminal.shell", &config);
        assert!(screen.open_select().is_some());
        assert_eq!(
            screen.pick("terminal.shell", "auto", &config),
            Act::None,
            "already auto"
        );
        assert_eq!(
            screen.pick("terminal.shell", "git-bash", &config),
            Act::Write("terminal.shell", SettingValue::Text("git-bash".into()))
        );
    }

    #[test]
    fn rf_cfg_05_a_written_value_shows_until_the_file_is_read() {
        let config = Config::default();
        let mut screen = Screen::default();
        screen.written("ui.font.size", SettingValue::Number(17.0), &config);
        assert_eq!(
            screen.shown("ui.font.size", &config),
            SettingValue::Number(17.0)
        );
        // A second click steps from what is shown, not from the file.
        assert_eq!(
            screen.click_step("ui.font.size", true, &config),
            Act::Write("ui.font.size", SettingValue::Number(18.0))
        );
        screen.file_read(&config);
        assert_eq!(
            screen.shown("ui.font.size", &config),
            SettingValue::Number(17.0),
            "the file still has the old value: the write is queued"
        );
        screen.file_read(&self::config("ui.font.size = 17"));
        assert_eq!(
            screen.shown("ui.font.size", &config),
            SettingValue::Number(13.0)
        );
        screen.written("theme", SettingValue::Text("washi".into()), &config);
        screen.write_failed("theme");
        assert_eq!(
            screen.shown("theme", &config),
            SettingValue::Text("sumi".into())
        );
    }

    #[test]
    fn rf_cfg_05_a_reading_drops_only_the_writes_it_confirms() {
        let at_13 = Config::default();
        let size = |n: f32| SettingValue::Number(n);
        let mut screen = Screen::default();
        for n in [14.0, 15.0] {
            screen.written("ui.font.size", size(n), &at_13);
        }
        screen.written("theme", SettingValue::Text("washi".into()), &at_13);
        // The reading of the first write lands between the clicks.
        let at_14 = config("ui.font.size = 14");
        screen.file_read(&at_14);
        assert_eq!(screen.shown("ui.font.size", &at_14), size(15.0));
        assert_eq!(
            screen.click_step("ui.font.size", true, &at_14),
            Act::Write("ui.font.size", size(16.0)),
            "the third click steps from the last written value"
        );
        assert_eq!(
            screen.shown("theme", &at_14),
            SettingValue::Text("washi".into()),
            "another key's write keeps waiting"
        );
        // A hand edit to another value wins over what is waiting.
        let by_hand = config("ui.font.size = 19\ntheme = \"system\"");
        screen.file_read(&by_hand);
        assert_eq!(screen.shown("ui.font.size", &by_hand), size(19.0));
        assert_eq!(
            screen.shown("theme", &by_hand),
            SettingValue::Text("system".into())
        );
    }

    #[test]
    fn sizes_read_as_pixels() {
        assert_eq!(size_label(&SettingValue::Number(13.0)), "13 px");
        assert_eq!(size_label(&SettingValue::Number(14.5)), "14.5 px");
    }
}
