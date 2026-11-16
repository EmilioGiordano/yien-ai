//! The test control channel (ADR-21): what a script sends to a YIEN started
//! with `--test-control <name>`, and what it gets back. One JSON object per
//! line each way, one reply per request.
//!
//! ```
//! use yien_proto::test_control::{Request, Reply};
//!
//! let request = Request::State { text: false };
//! assert!(request.check().is_ok());
//! assert!(Reply::error("no such pane").error.is_some());
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{PaneId, ProfileId, ProjectId, TabId};

/// Version of the test control messages, reported in every [`UiState`].
///
/// ```
/// assert!(yien_proto::test_control::TEST_CONTROL_VERSION >= 1);
/// ```
pub const TEST_CONTROL_VERSION: u32 = 1;

/// The longest request line, in bytes, without its newline.
///
/// ```
/// assert!(yien_proto::test_control::MAX_REQUEST_LEN >= yien_proto::test_control::MAX_TEXT_LEN);
/// ```
pub const MAX_REQUEST_LEN: usize = 64 * 1024;

/// The most text one [`Request::Text`] types, in bytes.
///
/// ```
/// assert_eq!(yien_proto::test_control::MAX_TEXT_LEN, 16 * 1024);
/// ```
pub const MAX_TEXT_LEN: usize = 16 * 1024;

/// The longest key name, command id or profile id a request carries.
///
/// ```
/// assert_eq!(yien_proto::test_control::MAX_NAME_LEN, 64);
/// ```
pub const MAX_NAME_LEN: usize = 64;

/// The smallest and largest window size [`Request::Resize`] accepts, in
/// logical pixels.
///
/// ```
/// use yien_proto::test_control::{MAX_WINDOW_SIDE, MIN_WINDOW_SIDE};
/// assert!(MIN_WINDOW_SIDE < MAX_WINDOW_SIDE);
/// ```
pub const MIN_WINDOW_SIDE: f32 = 100.0;
/// See [`MIN_WINDOW_SIDE`].
pub const MAX_WINDOW_SIDE: f32 = 16_384.0;

/// One request, tagged by `op`.
///
/// ```
/// use yien_proto::test_control::Request;
///
/// let key = Request::Key { pane: None, key: "ctrl-c".into() };
/// assert!(key.check().is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// The window's state; `text` adds each pane's visible rows.
    State {
        /// Include the visible rows of every pane that has a frame.
        #[serde(default)]
        text: bool,
    },
    /// Runs a command as the palette does: a catalog id, or one of the
    /// palette's rows that take arguments.
    Command {
        /// The command id: `pane.split_right`, `tab.new_with`.
        id: String,
        /// What the command acts on, for the ids that take arguments.
        #[serde(default)]
        args: CommandArgs,
    },
    /// Types text into the pane that has the keyboard, as typed characters.
    Text {
        /// The pane that must have the keyboard; any pane when `None`.
        #[serde(default)]
        pane: Option<PaneId>,
        /// What to type: printable characters only.
        text: TypedText,
    },
    /// Presses one key, in gpui notation: `ctrl-c`, `enter`, `escape`, `up`.
    Key {
        /// The pane that must have the keyboard; any pane when `None`.
        #[serde(default)]
        pane: Option<PaneId>,
        /// The key with its modifiers.
        key: String,
    },
    /// Resizes the window's content area, in logical pixels.
    Resize {
        /// The new width.
        width: f32,
        /// The new height.
        height: f32,
    },
}

impl Request {
    /// The request's `op`, for logs: never its text.
    ///
    /// ```
    /// use yien_proto::test_control::Request;
    /// assert_eq!(Request::State { text: true }.op(), "state");
    /// ```
    pub fn op(&self) -> &'static str {
        match self {
            Self::State { .. } => "state",
            Self::Command { .. } => "command",
            Self::Text { .. } => "text",
            Self::Key { .. } => "key",
            Self::Resize { .. } => "resize",
        }
    }

    /// Checks the sizes and values serde cannot: lengths, control characters
    /// in text, a window size in range.
    ///
    /// ```
    /// use yien_proto::test_control::{Request, TypedText};
    ///
    /// assert!(Request::Text { pane: None, text: TypedText::new("ls") }.check().is_ok());
    /// assert!(Request::Text { pane: None, text: TypedText::new("ls\r") }.check().is_err());
    /// assert!(Request::Resize { width: f32::NAN, height: 600.0 }.check().is_err());
    /// ```
    pub fn check(&self) -> Result<(), String> {
        match self {
            Self::State { .. } => Ok(()),
            Self::Command { id, args } => {
                name("command id", id)?;
                if let Some(profile) = &args.profile {
                    name("profile", profile.as_str())?;
                }
                Ok(())
            }
            Self::Text { text, .. } => {
                let text = text.as_str();
                if text.is_empty() {
                    return Err("text is empty".to_owned());
                }
                if text.len() > MAX_TEXT_LEN {
                    return Err(format!("text is longer than {MAX_TEXT_LEN} bytes"));
                }
                if text.chars().any(char::is_control) {
                    return Err(
                        "text has a control character; press enter, tab or escape with key"
                            .to_owned(),
                    );
                }
                Ok(())
            }
            Self::Key { key, .. } => name("key", key),
            Self::Resize { width, height } => {
                for side in [width, height] {
                    if !side.is_finite() || !(MIN_WINDOW_SIDE..=MAX_WINDOW_SIDE).contains(side) {
                        return Err(format!(
                            "window sides must be between {MIN_WINDOW_SIDE} and {MAX_WINDOW_SIDE}"
                        ));
                    }
                }
                Ok(())
            }
        }
    }
}

fn name(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if value.len() > MAX_NAME_LEN {
        return Err(format!("{what} is longer than {MAX_NAME_LEN} bytes"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{what} has a control character"));
    }
    Ok(())
}

/// What a [`Request::Command`] acts on. Each id names the fields it reads;
/// the others are ignored.
///
/// ```
/// let args: yien_proto::test_control::CommandArgs = Default::default();
/// assert!(args.profile.is_none());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandArgs {
    /// The profile a new tab or a split starts.
    #[serde(default)]
    pub profile: Option<ProfileId>,
    /// Which way a split goes: `right` or `down`.
    #[serde(default)]
    pub direction: Option<SplitWay>,
    /// The pane to focus.
    #[serde(default)]
    pub pane: Option<PaneId>,
    /// The tab to select.
    #[serde(default)]
    pub tab: Option<TabId>,
    /// The project to select.
    #[serde(default)]
    pub project: Option<ProjectId>,
}

/// Which way a split with a profile goes, as the split menu offers.
///
/// ```
/// let way: yien_proto::test_control::SplitWay = serde_json::from_str("\"down\"").unwrap();
/// assert_eq!(way, yien_proto::test_control::SplitWay::Down);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitWay {
    /// The new pane on the right.
    Right,
    /// The new pane below.
    Down,
}

/// Text a request types. Its `Debug` shows only the length: typed text may
/// be a prompt, which never reaches a log.
///
/// ```
/// let text = yien_proto::test_control::TypedText::new("secret");
/// assert_eq!(format!("{text:?}"), "TypedText(6 bytes)");
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TypedText(String);

impl TypedText {
    /// Wraps text.
    ///
    /// ```
    /// assert_eq!(yien_proto::test_control::TypedText::new("ls").as_str(), "ls");
    /// ```
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The text.
    ///
    /// ```
    /// assert!(yien_proto::test_control::TypedText::new("").as_str().is_empty());
    /// ```
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TypedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TypedText({} bytes)", self.0.len())
    }
}

/// The answer to one request: `ok`, and what failed or what was asked for.
///
/// ```
/// use yien_proto::test_control::Reply;
///
/// let json = serde_json::to_string(&Reply::done()).unwrap();
/// assert_eq!(json, r#"{"ok":true}"#);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    /// The request was carried out.
    pub ok: bool,
    /// Why it was not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The window's state, for [`Request::State`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Box<UiState>>,
    /// For [`Request::Key`]: whether a binding or the pane took the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handled: Option<bool>,
}

impl Reply {
    /// Carried out, nothing to report.
    ///
    /// ```
    /// assert!(yien_proto::test_control::Reply::done().ok);
    /// ```
    pub fn done() -> Self {
        Self {
            ok: true,
            error: None,
            state: None,
            handled: None,
        }
    }

    /// Not carried out, and why.
    ///
    /// ```
    /// assert!(!yien_proto::test_control::Reply::error("busy").ok);
    /// ```
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            ..Self::done()
        }
    }

    /// The window's state.
    ///
    /// ```
    /// let reply = yien_proto::test_control::Reply::state(Default::default());
    /// assert!(reply.ok && reply.state.is_some());
    /// ```
    pub fn state(state: UiState) -> Self {
        Self {
            state: Some(Box::new(state)),
            ..Self::done()
        }
    }

    /// A key was pressed; `handled` says whether anything took it.
    ///
    /// ```
    /// assert_eq!(yien_proto::test_control::Reply::key(true).handled, Some(true));
    /// ```
    pub fn key(handled: bool) -> Self {
        Self {
            handled: Some(handled),
            ..Self::done()
        }
    }
}

/// Everything the window shows that a check needs.
///
/// ```
/// let state = yien_proto::test_control::UiState::default();
/// assert_eq!(state.waiting, 0);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UiState {
    /// [`TEST_CONTROL_VERSION`].
    pub version: u32,
    /// The window itself.
    pub window: WindowState,
    /// The pane whose terminal has the keyboard, if one does.
    pub keyboard_pane: Option<PaneId>,
    /// The selected tab.
    pub selected_tab: Option<TabId>,
    /// Panes that need the user: the counter in the title bar (RF-ATN-05).
    pub waiting: usize,
    /// Projects in tab bar order.
    pub projects: Vec<ProjectState>,
    /// Tabs in tab bar order, project by project.
    pub tabs: Vec<TabState>,
    /// Panes, tab by tab, each tab's in layout order.
    pub panes: Vec<PaneState>,
}

/// The window's size and focus.
///
/// ```
/// assert!(!yien_proto::test_control::WindowState::default().active);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// The content area's width, in logical pixels.
    pub width: f32,
    /// The content area's height, in logical pixels.
    pub height: f32,
    /// The OS says the window is the active one.
    pub active: bool,
    /// Where the keyboard is: `panes`, `tab_bar`, `context_bar`, `dividers`.
    pub region: String,
    /// The overlay that holds the keyboard, if any: `palette`, `confirm`.
    pub overlay: Option<String>,
}

/// One project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectState {
    /// Its id.
    pub id: ProjectId,
    /// The name in the tab bar.
    pub name: String,
    /// Its folder.
    pub root: String,
    /// Its tabs are folded away.
    pub folded: bool,
    /// Its tabs, in tab bar order.
    pub tabs: Vec<TabId>,
}

/// One tab, as its row shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabState {
    /// Its id.
    pub id: TabId,
    /// Its project.
    pub project: ProjectId,
    /// The name in the tab bar.
    pub name: String,
    /// The folder its panes start in.
    pub dir: String,
    /// The git branch checked out there.
    pub branch: Option<String>,
    /// It was opened on a linked worktree.
    pub worktree: bool,
    /// The state word of its most urgent pane: `Working`, `Needs you`.
    pub state: String,
    /// The row's mark: the snake case state (`needs_you`, `working`), or
    /// `shell` for a tab of shells with nothing to report (no mark drawn).
    pub mark: String,
    /// The row carries the red bar (RN-01).
    pub needs_you: bool,
    /// Its notifications are muted.
    pub muted: bool,
    /// The pane focused in it.
    pub focused: PaneId,
    /// The pane shown alone, if any.
    pub maximized: Option<PaneId>,
    /// Its panes, in layout order.
    pub panes: Vec<PaneId>,
}

/// One pane, as its head shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneState {
    /// Its id.
    pub id: PaneId,
    /// Its tab.
    pub tab: TabId,
    /// What runs in it: `claude`, `pwsh`.
    pub name: String,
    /// The profile's name: `Claude Code`, `Shell`.
    pub profile: String,
    /// The profile's id: `claude`, `shell`.
    pub profile_id: String,
    /// The program tag the head shows next to the name.
    pub tag: String,
    /// The state word: `Idle`, `Working`, `Needs you`.
    pub state: String,
    /// The agent's state cannot be told (RN-02).
    pub signal_lost: bool,
    /// Its program runs.
    pub running: bool,
    /// Its program ran and stopped.
    pub exited: bool,
    /// The exit code, once it stopped.
    pub exit_code: Option<u32>,
    /// The folder its shell last reported.
    pub cwd: Option<String>,
    /// It is its tab's focused pane.
    pub focused: bool,
    /// Its terminal has the keyboard.
    pub keyboard: bool,
    /// It is drawn: in the selected tab, and not hidden by a maximized one.
    pub visible: bool,
    /// The terminal size of its last frame: columns, rows.
    pub size: Option<(u16, u16)>,
    /// Millis since the UNIX epoch when its terminal view took the last
    /// frame, `None` before the first frame. Test-only precision for the
    /// test control dump: scripts compare it against their own clock to
    /// time key-to-screen without poll quantization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_at_ms: Option<u64>,
    /// Its visible rows, when the request asked for text and a frame came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<VisibleRows>,
}

/// A pane's visible rows, top to bottom. Its `Debug` shows only the count:
/// the screen never reaches a log.
///
/// ```
/// let rows = yien_proto::test_control::VisibleRows::new(vec!["PS C:\\>".into()]);
/// assert_eq!(format!("{rows:?}"), "VisibleRows(1 rows)");
/// ```
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VisibleRows(Vec<String>);

impl VisibleRows {
    /// Wraps rows.
    ///
    /// ```
    /// assert!(yien_proto::test_control::VisibleRows::new(Vec::new()).as_slice().is_empty());
    /// ```
    pub fn new(rows: Vec<String>) -> Self {
        Self(rows)
    }

    /// The rows.
    ///
    /// ```
    /// let rows = yien_proto::test_control::VisibleRows::new(vec!["a".into()]);
    /// assert_eq!(rows.as_slice(), ["a"]);
    /// ```
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl fmt::Debug for VisibleRows {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VisibleRows({} rows)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> PaneState {
        PaneState {
            id: PaneId::new(3),
            tab: TabId::new(2),
            name: "pwsh".into(),
            profile: "Shell".into(),
            profile_id: "shell".into(),
            tag: "pwsh".into(),
            state: "Idle".into(),
            signal_lost: false,
            running: true,
            exited: false,
            exit_code: None,
            cwd: None,
            focused: true,
            keyboard: true,
            visible: true,
            size: None,
            frame_at_ms: None,
            rows: None,
        }
    }

    fn parse(json: &str) -> Result<Request, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn adr_21_requests_parse_from_their_documented_lines() {
        assert_eq!(
            parse(r#"{"op":"state"}"#).unwrap(),
            Request::State { text: false }
        );
        assert_eq!(
            parse(r#"{"op":"state","text":true}"#).unwrap(),
            Request::State { text: true }
        );
        assert_eq!(
            parse(r#"{"op":"command","id":"pane.split_right"}"#).unwrap(),
            Request::Command {
                id: "pane.split_right".into(),
                args: CommandArgs::default()
            }
        );
        assert_eq!(
            parse(r#"{"op":"command","id":"pane.split_with","args":{"profile":"claude","direction":"down"}}"#)
                .unwrap(),
            Request::Command {
                id: "pane.split_with".into(),
                args: CommandArgs {
                    profile: Some(ProfileId::new("claude")),
                    direction: Some(SplitWay::Down),
                    ..CommandArgs::default()
                }
            }
        );
        assert_eq!(
            parse(r#"{"op":"text","pane":3,"text":"claude"}"#).unwrap(),
            Request::Text {
                pane: Some(PaneId::new(3)),
                text: TypedText::new("claude")
            }
        );
        assert_eq!(
            parse(r#"{"op":"key","key":"ctrl-c"}"#).unwrap(),
            Request::Key {
                pane: None,
                key: "ctrl-c".into()
            }
        );
        assert_eq!(
            parse(r#"{"op":"resize","width":1200,"height":800}"#).unwrap(),
            Request::Resize {
                width: 1200.0,
                height: 800.0
            }
        );
    }

    #[test]
    fn adr_21_unknown_ops_fields_and_types_are_refused() {
        for bad in [
            r#"{"op":"format_disk"}"#,
            r#"{"op":"state","txet":true}"#,
            r#"{"op":"key","key":"enter","pnae":1}"#,
            r#"{"op":"command","id":"x","args":{"profle":"claude"}}"#,
            r#"{"op":"command","id":"x","args":{"direction":"diagonal"}}"#,
            r#"{"op":"text","pane":-1,"text":"a"}"#,
            r#"{"op":"text","pane":"1","text":"a"}"#,
            r#"{"text":"a"}"#,
            r#"[]"#,
            r#"null"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn adr_21_checks_refuse_what_serde_lets_through() {
        let long = "a".repeat(MAX_TEXT_LEN + 1);
        let refused = [
            Request::Text {
                pane: None,
                text: TypedText::new(long),
            },
            Request::Text {
                pane: None,
                text: TypedText::new(""),
            },
            Request::Text {
                pane: None,
                text: TypedText::new("a\u{1b}[31m"),
            },
            Request::Key {
                pane: None,
                key: String::new(),
            },
            Request::Key {
                pane: None,
                key: "k".repeat(MAX_NAME_LEN + 1),
            },
            Request::Command {
                id: "pane.close\n".into(),
                args: CommandArgs::default(),
            },
            Request::Command {
                id: "tab.new_with".into(),
                args: CommandArgs {
                    profile: Some(ProfileId::new("")),
                    ..CommandArgs::default()
                },
            },
            Request::Resize {
                width: 99.0,
                height: 600.0,
            },
            Request::Resize {
                width: 800.0,
                height: f32::INFINITY,
            },
        ];
        for request in refused {
            assert!(request.check().is_err(), "{request:?}");
        }
        let text = Request::Text {
            pane: None,
            text: TypedText::new("a".repeat(MAX_TEXT_LEN)),
        };
        assert_eq!(text.check(), Ok(()));
    }

    #[test]
    fn adr_21_state_serialises_as_documented_and_round_trips() {
        let state = UiState {
            version: TEST_CONTROL_VERSION,
            window: WindowState {
                width: 1440.0,
                height: 900.0,
                active: false,
                region: "panes".into(),
                overlay: None,
            },
            keyboard_pane: Some(PaneId::new(3)),
            selected_tab: Some(TabId::new(2)),
            waiting: 1,
            projects: vec![ProjectState {
                id: ProjectId::new(1),
                name: "work".into(),
                root: "C:/work".into(),
                folded: false,
                tabs: vec![TabId::new(2)],
            }],
            tabs: vec![TabState {
                id: TabId::new(2),
                project: ProjectId::new(1),
                name: "Tab 1".into(),
                dir: "C:/work".into(),
                branch: None,
                worktree: false,
                state: "Needs you".into(),
                mark: "needs_you".into(),
                needs_you: true,
                muted: false,
                focused: PaneId::new(3),
                maximized: None,
                panes: vec![PaneId::new(3)],
            }],
            panes: vec![PaneState {
                name: "claude".into(),
                state: "Needs you".into(),
                rows: Some(VisibleRows::new(vec!["> hi".into()])),
                size: Some((80, 24)),
                ..pane()
            }],
        };
        let json = serde_json::to_value(Reply::state(state.clone())).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["state"]["keyboard_pane"], 3);
        assert_eq!(json["state"]["tabs"][0]["mark"], "needs_you");
        assert_eq!(json["state"]["panes"][0]["rows"][0], "> hi");
        assert_eq!(json["state"]["panes"][0]["size"][0], 80);
        let back: Reply = serde_json::from_value(json).unwrap();
        assert_eq!(back.state.as_deref(), Some(&state));
    }

    #[test]
    fn rows_are_left_out_unless_asked_for() {
        let json = serde_json::to_value(pane()).unwrap();
        assert!(json.get("rows").is_none());
    }

    #[test]
    fn frame_at_ms_is_left_out_unless_a_frame_was_set() {
        let json = serde_json::to_value(pane()).unwrap();
        assert!(json.get("frame_at_ms").is_none());
        let stamped = PaneState {
            frame_at_ms: Some(1_700_000_000_000),
            ..pane()
        };
        let json = serde_json::to_value(&stamped).unwrap();
        assert_eq!(json["frame_at_ms"], 1_700_000_000_000u64);
        let back: PaneState = serde_json::from_value(json).unwrap();
        assert_eq!(back.frame_at_ms, Some(1_700_000_000_000));
    }

    #[test]
    fn debug_never_shows_typed_text_or_the_screen() {
        let request = Request::Text {
            pane: None,
            text: TypedText::new("my secret prompt"),
        };
        assert!(!format!("{request:?}").contains("secret"));
        let pane = PaneState {
            rows: Some(VisibleRows::new(vec!["my secret screen".into()])),
            ..pane()
        };
        assert!(!format!("{pane:?}").contains("secret"));
    }
}
