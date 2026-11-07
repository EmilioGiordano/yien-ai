//! The messages themselves: commands from the UI ([`ClientMsg`], [`Command`])
//! and events from the core ([`CoreMsg`]), with the workspace descriptions they
//! carry.
//!
//! State events carry the whole object (a [`PaneInfo`], a [`TabInfo`]), never
//! a delta, so a lost or merged event is corrected by the next one.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::frame::{Frame, SelectionKind, Side, ViewportPoint};
use crate::{PaneId, ProjectId, TabId};

/// Names an agent profile: `claude`, `codex`, `shell` (RF-AGE-01, RF-PAN-18).
///
/// ```
/// let id = yien_proto::ProfileId::new("claude");
/// assert_eq!(id.as_str(), "claude");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(String);

impl ProfileId {
    /// Wraps a profile id.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bytes for a pane's program. Its `Debug` shows only the length, so typed
/// text never reaches a log (plan §5.4).
///
/// ```
/// let bytes = yien_proto::InputBytes::new(b"ls\r".to_vec());
/// assert_eq!(format!("{bytes:?}"), "InputBytes(3 bytes)");
/// assert_eq!(bytes.as_slice(), b"ls\r");
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputBytes(Vec<u8>);

impl InputBytes {
    /// Wraps bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// The bytes, unwrapped.
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

impl fmt::Debug for InputBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InputBytes({} bytes)", self.0.len())
    }
}

/// How a split lays out its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Orientation {
    /// Side by side, left to right.
    Horizontal,
    /// Stacked, top to bottom.
    Vertical,
}

/// A direction to move the focus in (RF-PAN-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Towards the left.
    Left,
    /// Towards the right.
    Right,
    /// Upwards.
    Up,
    /// Downwards.
    Down,
}

/// The side of a pane another pane is docked on (RF-PAN-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DropZone {
    /// The left half.
    Left,
    /// The right half.
    Right,
    /// The top half.
    Top,
    /// The bottom half.
    Bottom,
}

/// Why an agent needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NeedsReason {
    /// A permission prompt.
    Permission,
    /// A question or an input request.
    Question,
    /// The agent reported it is blocked.
    Blocked,
    /// A screen rule confirmed it.
    ScreenRule,
}

/// Why an agent is in error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorReason {
    /// The turn ended on an API error.
    TurnFailed,
    /// The process exited in the middle of a turn.
    UnexpectedExit {
        /// Its exit code, when there is one. Windows codes span the full
        /// `u32` range (`0xC000013A` is Ctrl+C).
        code: Option<u32>,
    },
}

/// What the agent in a pane is doing (RF-AGE-05).
///
/// ```
/// use yien_proto::{AgentState, NeedsReason};
///
/// assert!(AgentState::NeedsYou(NeedsReason::Permission).is_red());
/// assert!(!AgentState::Working.is_red());
/// assert_eq!(AgentState::default(), AgentState::Idle);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum AgentState {
    /// Inactive, and everything that cannot be determined (RN-02).
    #[default]
    Idle,
    /// A turn is running.
    Working,
    /// The agent waits for the user; the only red state (RN-01).
    NeedsYou(NeedsReason),
    /// A turn completed while the pane was not seen.
    Done,
    /// The turn or the process failed.
    Error(ErrorReason),
}

impl AgentState {
    /// Whether this state carries the red mark (RN-01).
    pub fn is_red(self) -> bool {
        matches!(self, AgentState::NeedsYou(_))
    }
}

/// The command catalog (ADR-19): every workspace action the palette, the
/// keymap and the future CLI can trigger. Targets are explicit; the UI fills
/// in the focused pane or the selected tab.
///
/// ```
/// use yien_proto::{Command, PaneId};
///
/// let close = Command::ClosePane { pane: PaneId::new(3) };
/// assert_ne!(close, Command::FocusPane { pane: PaneId::new(3) });
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Registers a project from a local folder and opens a tab on its main
    /// working copy (RF-ESP-01, RF-ESP-06).
    NewProject {
        /// The project folder.
        root: String,
        /// The name in the tab bar; the folder name when `None`.
        name: Option<String>,
    },
    /// Opens a tab on the project's main working copy, with one shell pane
    /// (RF-ESP-03, RF-ESP-06).
    NewTab {
        /// The project.
        project: ProjectId,
        /// The name in the tab bar; generated when `None`.
        name: Option<String>,
    },
    /// Closes a tab and terminates its panes (RF-ESP-03).
    CloseTab {
        /// The tab.
        tab: TabId,
    },
    /// Makes a tab the selected one.
    SelectTab {
        /// The tab.
        tab: TabId,
    },
    /// Splits a pane and starts a program in the new one (RF-PAN-01,
    /// RF-PAN-18).
    SplitPane {
        /// The pane to split; the new pane goes after it.
        pane: PaneId,
        /// Side by side or stacked.
        orientation: Orientation,
        /// The profile to launch; the user's shell when `None`.
        profile: Option<ProfileId>,
    },
    /// Closes a pane and terminates its program. Closing the last pane of a
    /// tab closes the tab.
    ClosePane {
        /// The pane.
        pane: PaneId,
    },
    /// Gives a pane the keyboard and selects its tab (RF-PAN-03, RF-PAN-17).
    FocusPane {
        /// The pane.
        pane: PaneId,
    },
    /// Moves the focus to the neighbouring pane in the selected tab
    /// (RF-PAN-03).
    FocusDirection {
        /// Where to look.
        direction: Direction,
    },
    /// Grows a pane by a fraction of its split, at its siblings' cost
    /// (RF-PAN-02).
    ResizeSplit {
        /// The pane.
        pane: PaneId,
        /// The fraction to add; negative shrinks.
        delta: f32,
    },
    /// Exchanges two panes of the same tab; their programs keep running
    /// (RF-PAN-05).
    SwapPanes {
        /// One pane.
        a: PaneId,
        /// The other pane.
        b: PaneId,
    },
    /// Docks a pane on one side of another pane of the same tab (RF-PAN-05).
    MovePane {
        /// The pane that moves.
        pane: PaneId,
        /// The pane it docks against.
        target: PaneId,
        /// The side.
        zone: DropZone,
    },
    /// Maximizes a pane inside its tab, or restores the layout when it is
    /// already maximized (RF-PAN-04).
    ToggleMaximize {
        /// The pane.
        pane: PaneId,
    },
    /// Renames a pane (RF-PAN-20).
    RenamePane {
        /// The pane.
        pane: PaneId,
        /// The new name; empty goes back to the default name.
        name: String,
    },
    /// Starts the pane's program again, terminating it first if it still
    /// runs (RF-PAN-19, RF-PAN-20).
    RelaunchPane {
        /// The pane.
        pane: PaneId,
    },
    /// Terminates the pane's program and keeps the pane (RF-PAN-20).
    TerminatePane {
        /// The pane.
        pane: PaneId,
    },
}

/// Everything the UI sends to the core: catalog commands, plus the
/// high-frequency terminal traffic that is not a command.
///
/// ```
/// use yien_proto::{ClientMsg, InputBytes, PaneId};
///
/// let msg = ClientMsg::Input { pane: PaneId::new(1), bytes: InputBytes::new(b"ls\r".to_vec()) };
/// assert!(format!("{msg:?}").contains("3 bytes"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// A workspace command.
    Command(Command),
    /// The user's own keys or paste, written to the program unaltered
    /// (RF-AGE-02).
    Input {
        /// The pane.
        pane: PaneId,
        /// The bytes, already encoded for the terminal mode.
        bytes: InputBytes,
    },
    /// Bytes YIEN generates itself (focus and mouse reports). Written like
    /// input, but they do not count as the user acting on the pane.
    Report {
        /// The pane.
        pane: PaneId,
        /// The bytes.
        bytes: InputBytes,
    },
    /// The pane's area changed size, in cells.
    ResizePane {
        /// The pane.
        pane: PaneId,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Scrolls the pane's viewport; positive goes back into the history.
    Scroll {
        /// The pane.
        pane: PaneId,
        /// Lines.
        delta: i32,
    },
    /// Returns the pane's viewport to the live screen.
    ScrollToBottom {
        /// The pane.
        pane: PaneId,
    },
    /// Starts a selection, replacing any previous one.
    StartSelection {
        /// The pane.
        pane: PaneId,
        /// The cell under the pointer.
        at: ViewportPoint,
        /// Which half of the cell.
        side: Side,
        /// How it grows.
        kind: SelectionKind,
    },
    /// Extends the selection.
    UpdateSelection {
        /// The pane.
        pane: PaneId,
        /// The cell under the pointer.
        at: ViewportPoint,
        /// Which half of the cell.
        side: Side,
    },
    /// Drops the selection.
    ClearSelection {
        /// The pane.
        pane: PaneId,
    },
    /// Asks for the selected text; answered with [`CoreMsg::SelectionText`].
    CopySelection {
        /// The pane.
        pane: PaneId,
    },
    /// The window gained or lost the focus. A pane is seen only when it has
    /// the focus in a focused window (RN-07).
    WindowFocus {
        /// Whether the window has the focus.
        focused: bool,
    },
}

/// One child of a split, with its share of the space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutChild {
    /// Its share of the split; the shares of a split sum to 1.
    pub flex: f32,
    /// What it holds.
    pub node: LayoutNode,
}

/// How a tab's panes are laid out.
///
/// ```
/// use yien_proto::{LayoutChild, LayoutNode, Orientation, PaneId};
///
/// let layout = LayoutNode::Split {
///     orientation: Orientation::Horizontal,
///     children: vec![
///         LayoutChild { flex: 0.5, node: LayoutNode::Pane(PaneId::new(1)) },
///         LayoutChild { flex: 0.5, node: LayoutNode::Pane(PaneId::new(2)) },
///     ],
/// };
/// assert_eq!(layout.panes(), [PaneId::new(1), PaneId::new(2)]);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayoutNode {
    /// A single pane.
    Pane(PaneId),
    /// Children laid out along one axis.
    Split {
        /// The axis.
        orientation: Orientation,
        /// The children, left to right or top to bottom.
        children: Vec<LayoutChild>,
    },
}

impl LayoutNode {
    /// Every pane, left to right and top to bottom.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            LayoutNode::Pane(pane) => out.push(*pane),
            LayoutNode::Split { children, .. } => {
                for child in children {
                    child.node.collect(out);
                }
            }
        }
    }
}

/// A project as the tab bar shows it (RF-ESP-02).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// Its id.
    pub id: ProjectId,
    /// The name in the tab bar.
    pub name: String,
    /// The folder it was registered from.
    pub root: String,
    /// Its tabs, in tab-bar order.
    pub tabs: Vec<TabId>,
}

/// A tab and its layout (RF-ESP-04).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    /// Its id.
    pub id: TabId,
    /// The project it belongs to.
    pub project: ProjectId,
    /// The name in the tab bar.
    pub name: String,
    /// The working directory its panes start in.
    pub dir: String,
    /// The git branch checked out in `dir`; `None` until git support lands.
    pub branch: Option<String>,
    /// How its panes are laid out.
    pub layout: LayoutNode,
    /// The pane with the keyboard; always one of `layout`'s panes.
    pub focused: PaneId,
    /// The maximized pane, if any (RF-PAN-04).
    pub maximized: Option<PaneId>,
}

/// A pane as its title bar shows it (RF-PAN-12, RF-PAN-16, RF-PAN-20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    /// Its id.
    pub id: PaneId,
    /// The tab it belongs to.
    pub tab: TabId,
    /// The name the user gave it, or the program's name.
    pub name: String,
    /// The display name of the profile it runs: `Claude Code`, `Shell`.
    pub profile: String,
    /// The title the program set, if any.
    pub title: Option<String>,
    /// The working directory the shell last reported, if any.
    pub cwd: Option<String>,
    /// What its agent is doing.
    pub state: AgentState,
    /// Whether its program is running.
    pub running: bool,
    /// The exit code of the program's last run, once it exited.
    pub exit_code: Option<u32>,
    /// The agent's session id, once known (RF-PAN-20).
    pub session_id: Option<String>,
}

/// The whole workspace, sent as the core's first event.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    /// Every project, in tab-bar order.
    pub projects: Vec<ProjectInfo>,
    /// Every tab.
    pub tabs: Vec<TabInfo>,
    /// Every pane.
    pub panes: Vec<PaneInfo>,
    /// The selected tab.
    pub active_tab: Option<TabId>,
}

/// Everything the core tells the UI.
///
/// Frames are "last one wins": the core may skip one when the UI is behind,
/// and the next carries the current screen. Every other event is delivered.
/// Events for a pane or tab the UI no longer knows are stale and ignored.
///
/// ```
/// use yien_proto::{AgentState, CoreMsg, PaneId};
///
/// let msg = CoreMsg::PaneState { pane: PaneId::new(1), state: AgentState::Working };
/// assert!(matches!(msg, CoreMsg::PaneState { .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CoreMsg {
    /// The whole workspace.
    WorkspaceSnapshot(WorkspaceSnapshot),
    /// A project was added or changed.
    ProjectUpdated(ProjectInfo),
    /// A tab was added or changed: layout, focus, name, maximized pane.
    TabUpdated(TabInfo),
    /// A tab was closed, with its panes.
    TabClosed {
        /// The tab.
        tab: TabId,
    },
    /// The selected tab changed.
    ActiveTab {
        /// The tab, or `None` when no tab is left.
        tab: Option<TabId>,
    },
    /// A pane was added or its details changed.
    PaneUpdated(PaneInfo),
    /// A pane was closed.
    PaneClosed {
        /// The pane.
        pane: PaneId,
    },
    /// The visible rows of a pane (ADR-06: the UI never holds the grid).
    PaneFrame {
        /// The pane.
        pane: PaneId,
        /// What to draw.
        frame: Frame,
    },
    /// The agent state of a pane changed (RF-PAN-12).
    PaneState {
        /// The pane.
        pane: PaneId,
        /// The new state.
        state: AgentState,
    },
    /// The program of a pane exited; the pane stays and can be relaunched
    /// (RF-PAN-19).
    PaneExited {
        /// The pane.
        pane: PaneId,
        /// The exit code, when the OS reported one.
        code: Option<u32>,
    },
    /// The answer to [`ClientMsg::CopySelection`].
    SelectionText {
        /// The pane.
        pane: PaneId,
        /// The selected text, or `None` when nothing is selected.
        text: Option<String>,
    },
    /// Something failed. The message says what happened and what to do.
    Error {
        /// The pane it concerns, if any.
        pane: Option<PaneId>,
        /// For the user.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Cell, CellFlags, Modes, MouseTracking, Rgb, Size};

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json = serde_json::to_string(value).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn commands_round_trip() {
        let commands = [
            Command::NewProject {
                root: "C:/Code/yien".into(),
                name: None,
            },
            Command::SplitPane {
                pane: PaneId::new(3),
                orientation: Orientation::Vertical,
                profile: Some(ProfileId::new("claude")),
            },
            Command::ResizeSplit {
                pane: PaneId::new(3),
                delta: -0.25,
            },
            Command::MovePane {
                pane: PaneId::new(3),
                target: PaneId::new(4),
                zone: DropZone::Top,
            },
        ];
        for command in commands {
            assert_eq!(round_trip(&command), command);
        }
    }

    #[test]
    fn client_messages_round_trip() {
        let msgs = [
            ClientMsg::Input {
                pane: PaneId::new(1),
                bytes: InputBytes::new(vec![0x1b, b'[', b'A']),
            },
            ClientMsg::StartSelection {
                pane: PaneId::new(1),
                at: ViewportPoint::new(2, 3),
                side: Side::Left,
                kind: SelectionKind::Semantic,
            },
            ClientMsg::WindowFocus { focused: true },
        ];
        for msg in msgs {
            assert_eq!(round_trip(&msg), msg);
        }
    }

    #[test]
    fn core_messages_round_trip() {
        let cell = Cell {
            ch: 'a',
            zerowidth: vec!['\u{301}'],
            fg: Rgb::new(1, 2, 3),
            bg: Rgb::default(),
            flags: CellFlags::BOLD | CellFlags::WIDE,
        };
        let frame = Frame {
            size: Size { cols: 1, rows: 1 },
            lines: vec![vec![cell]],
            cursor: None,
            display_offset: 0,
            history_len: 7,
            background: Rgb::default(),
            cursor_color: Rgb::new(9, 9, 9),
            modes: Modes {
                bracketed_paste: true,
                mouse: MouseTracking::Drag,
                ..Modes::default()
            },
        };
        let tab = TabInfo {
            id: TabId::new(2),
            project: ProjectId::new(1),
            name: "main".into(),
            dir: "C:/Code/yien".into(),
            branch: None,
            layout: LayoutNode::Pane(PaneId::new(3)),
            focused: PaneId::new(3),
            maximized: None,
        };
        let msgs = [
            CoreMsg::PaneFrame {
                pane: PaneId::new(3),
                frame,
            },
            CoreMsg::TabUpdated(tab),
            CoreMsg::PaneState {
                pane: PaneId::new(3),
                state: AgentState::Error(ErrorReason::UnexpectedExit {
                    code: Some(0xC000_013A),
                }),
            },
        ];
        for msg in msgs {
            assert_eq!(round_trip(&msg), msg);
        }
    }

    #[test]
    fn layout_lists_nested_panes_in_order() {
        let pane = |n| LayoutNode::Pane(PaneId::new(n));
        let layout = LayoutNode::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                LayoutChild {
                    flex: 0.5,
                    node: LayoutNode::Split {
                        orientation: Orientation::Vertical,
                        children: vec![
                            LayoutChild {
                                flex: 0.5,
                                node: pane(1),
                            },
                            LayoutChild {
                                flex: 0.5,
                                node: pane(2),
                            },
                        ],
                    },
                },
                LayoutChild {
                    flex: 0.5,
                    node: pane(3),
                },
            ],
        };
        assert_eq!(
            layout.panes(),
            [PaneId::new(1), PaneId::new(2), PaneId::new(3)]
        );
    }

    #[test]
    fn red_is_only_needs_you() {
        assert!(AgentState::NeedsYou(NeedsReason::Question).is_red());
        for state in [
            AgentState::Idle,
            AgentState::Working,
            AgentState::Done,
            AgentState::Error(ErrorReason::TurnFailed),
        ] {
            assert!(!state.is_red());
        }
    }
}
