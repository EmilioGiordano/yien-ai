//! The messages themselves: commands from the UI ([`ClientMsg`], [`Command`])
//! and events from the core ([`CoreMsg`]), with the workspace descriptions they
//! carry.
//!
//! [`CoreMsg::ProjectUpdated`], [`CoreMsg::TabUpdated`] and
//! [`CoreMsg::PaneUpdated`] carry the whole object, so a later one corrects
//! anything missed. [`CoreMsg::PaneState`] and [`CoreMsg::PaneExited`] are
//! deltas: they carry only what changed, and the next [`PaneInfo`] carries it
//! too.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::frame::{SelectionKind, Side, ViewportPoint};
use crate::{PaneId, ProjectId, TabGit, TabId};

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

/// The title of a past agent session: the first prompt the user typed in
/// it, on one line. Its `Debug` shows only the length, so a prompt never
/// reaches a log (plan §5.4).
///
/// ```
/// let title = yien_proto::SessionTitle::new("fix the build");
/// assert_eq!(format!("{title:?}"), "SessionTitle(13 chars)");
/// assert_eq!(title.as_str(), "fix the build");
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionTitle(String);

impl SessionTitle {
    /// Wraps a title.
    pub fn new(title: impl Into<String>) -> Self {
        Self(title.into())
    }

    /// The title as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionTitle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionTitle({} chars)", self.0.chars().count())
    }
}

/// What an agent that needs the user asks, as its hook summarised it: one
/// line of at most 160 characters (RF-ATN-13). Its `Debug` shows only the
/// length, so the text never reaches a log (plan §5.4).
///
/// ```
/// let q = yien_proto::PendingQuestion::new("Permission to use Bash");
/// assert_eq!(format!("{q:?}"), "PendingQuestion(22 chars)");
/// assert_eq!(q.as_str(), "Permission to use Bash");
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PendingQuestion(String);

impl PendingQuestion {
    /// Wraps a question.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The question as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PendingQuestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PendingQuestion({} chars)", self.0.chars().count())
    }
}

/// A past session of an agent CLI, as its own files describe it
/// (RF-SES-07).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// The profile whose CLI owns it, which resumes it.
    pub profile: ProfileId,
    /// The CLI's session id.
    pub id: String,
    /// The first prompt typed, if one was found.
    pub title: Option<SessionTitle>,
    /// When it last changed, in milliseconds since the Unix epoch.
    pub modified_ms: u64,
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
    /// working copy (RF-ESP-01, RF-ESP-06). A folder that is already a
    /// project ([`crate::same_folder`]) selects that project instead.
    NewProject {
        /// The project folder.
        root: String,
        /// The name in the tab bar; the folder name when `None`.
        name: Option<String>,
    },
    /// Opens a tab on the project's main working copy, with one pane
    /// (RF-ESP-03, RF-ESP-06, RF-PAN-18).
    NewTab {
        /// The project.
        project: ProjectId,
        /// The name in the tab bar; generated when `None`.
        name: Option<String>,
        /// The profile its pane launches; the user's shell when `None`.
        profile: Option<ProfileId>,
    },
    /// Removes a project from the sidebar, closing its tabs and terminating
    /// their panes. Nothing on disk is touched.
    CloseProject {
        /// The project.
        project: ProjectId,
    },
    /// Folds or unfolds a project's tabs in the sidebar (RF-ESP-17).
    SetProjectCollapsed {
        /// The project.
        project: ProjectId,
        /// Whether its tabs are hidden.
        collapsed: bool,
    },
    /// Renames a tab (RF-ESP-16).
    RenameTab {
        /// The tab.
        tab: TabId,
        /// The new name; empty goes back to the automatic name.
        name: String,
    },
    /// Moves a tab within its project's tabs (RF-ESP-16). A tab is never
    /// moved to another project: a `project` that is not the tab's own
    /// refuses the move.
    MoveTab {
        /// The tab.
        tab: TabId,
        /// The project the tab is in.
        project: ProjectId,
        /// Where the tab ends up among its project's tabs, counted from 0
        /// once it has left its old place; past the last tab means last.
        to_index: u32,
    },
    /// Mutes or unmutes a tab's toasts and taskbar flashes (RF-ATN-11). Its
    /// red mark, the counter and the badge stay (RF-ATN-16).
    SetTabMuted {
        /// The tab.
        tab: TabId,
        /// Whether its toasts and flashes are off.
        muted: bool,
    },
    /// Mutes or unmutes every toast and taskbar flash (RF-ATN-11).
    SetNotificationsMuted {
        /// Whether toasts and flashes are off.
        muted: bool,
    },
    /// Keeps the side bars' visibility and widths for the next start
    /// (RF-NAV-10). The UI draws them; the core only saves them.
    SetSideBars {
        /// The bars as the UI now draws them.
        bars: SideBars,
    },
    /// Keeps the terminals' zoom for the next start (RF-CFG-02). The UI
    /// applies it; the core only saves it.
    SetTerminalZoom {
        /// Pixels added to `terminal.font.size`, as the UI now draws it.
        zoom: i32,
    },
    /// Opens a tab on a new linked worktree with its own new branch
    /// (RF-ESP-05). The worktree goes beside the repository, in
    /// `<repo>-wt/<branch>`; a failure opens no tab and says why.
    NewWorktreeTab {
        /// The project whose repository gets the worktree.
        project: ProjectId,
        /// The new branch, which must not exist yet.
        branch: String,
        /// Where the branch starts: a branch, tag or commit; the project's
        /// current `HEAD` when `None`.
        base: Option<String>,
    },
    /// Asks what removing a worktree tab's worktree would lose; answered
    /// with [`CoreMsg::WorktreeChecked`] (RF-ESP-07, RN-08).
    CheckWorktree {
        /// The tab.
        tab: TabId,
    },
    /// Closes a worktree tab, terminating its panes, and then removes its
    /// worktree folder. The branch is never deleted. Without
    /// `discard_changes` a worktree with uncommitted changes is kept. Right
    /// before removing, git looks again: if the worktree holds more than
    /// `confirmed`, it is kept. A worktree another tab still uses is kept
    /// (RF-ESP-07, RN-08).
    RemoveWorktreeTab {
        /// The tab.
        tab: TabId,
        /// Delete uncommitted changes too; only after the user confirmed it.
        discard_changes: bool,
        /// What the user was shown and agreed to lose.
        confirmed: WorktreeWork,
    },
    /// Closes a tab and terminates its panes (RF-ESP-03).
    CloseTab {
        /// The tab.
        tab: TabId,
    },
    /// Makes a tab the selected one and focuses its pane that has needed the
    /// user longest, if any (RF-ATN-06).
    SelectTab {
        /// The tab.
        tab: TabId,
    },
    /// Splits a pane and starts a program in the new one (RF-PAN-01,
    /// RF-PAN-18).
    SplitPane {
        /// The pane to split.
        pane: PaneId,
        /// Side by side or stacked.
        orientation: Orientation,
        /// The profile to launch; the user's shell when `None`.
        profile: Option<ProfileId>,
        /// Puts the new pane before the split one (left of it or above it)
        /// instead of after it.
        #[serde(default)]
        before: bool,
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
    /// Moves the divider between two neighbouring children of a split, so
    /// the first takes `share` of the space the two hold together; the rest
    /// of the split stays put (RF-PAN-02). Ignored when the panes are not on
    /// the two sides of one divider, or `share` is not strictly between 0
    /// and 1; a share closer than [`crate::MIN_DIVIDER_SHARE`] to either end
    /// is clamped there.
    SetDivider {
        /// Any pane in the child before the divider.
        before: PaneId,
        /// Any pane in the child after it.
        after: PaneId,
        /// The first child's part of the pair's space.
        share: f32,
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
    /// Asks for the project's past agent sessions, read from each CLI's own
    /// files; always answered with [`CoreMsg::AgentSessions`], empty when the
    /// project is gone or nothing could be read (RF-SES-07).
    ListAgentSessions {
        /// The project.
        project: ProjectId,
        /// Chosen by the UI and sent back with the answer, so an answer to
        /// an earlier request can be told apart.
        request: u64,
    },
    /// Resumes a past agent session in a new tab on the project's folder
    /// (RF-SES-07). A session a pane already runs is focused instead.
    ResumeAgentSession {
        /// The project the session was listed for.
        project: ProjectId,
        /// The profile that resumes it.
        profile: ProfileId,
        /// The CLI's session id.
        session_id: String,
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
    /// Every pane, present and future, resolves its colours with this
    /// palette (RF-CFG-02). Screens and scrollback are kept.
    SetTerminalPalette {
        /// The palette.
        palette: TerminalPalette,
    },
    /// Whether shell panes started from now on report their folder at each
    /// prompt (RF-SES-01); running panes keep how they started.
    SetShellIntegration {
        /// On or off.
        on: bool,
    },
}

/// The terminal palette that goes with each window theme.
///
/// ```
/// use yien_proto::TerminalPalette;
///
/// assert_eq!(TerminalPalette::default(), TerminalPalette::Sumi);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TerminalPalette {
    /// Ink: light text on a dark grid.
    #[default]
    Sumi,
    /// Paper: dark text on a light grid.
    Washi,
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
    /// Whether its tabs are folded away in the sidebar (RF-ESP-17).
    pub collapsed: bool,
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
    /// The git branch checked out in `dir`, as last seen; `None` when
    /// detached, not a repository, or not looked at yet (RF-ESP-04).
    pub branch: Option<String>,
    /// How its panes are laid out.
    pub layout: LayoutNode,
    /// The pane with the keyboard; always one of `layout`'s panes.
    pub focused: PaneId,
    /// The maximized pane, if any (RF-PAN-04).
    pub maximized: Option<PaneId>,
    /// Its toasts and taskbar flashes are muted (RF-ATN-11).
    pub muted: bool,
    /// The linked worktree the tab was opened on, which is `dir`; `None`
    /// for a tab on the project's main working copy (RF-ESP-05).
    pub worktree: Option<String>,
}

/// What a worktree holds that a removal would delete or strand (RN-08):
/// the counts the question names, and a fingerprint of the files behind
/// them, so a removal can tell that nothing new arrived since the user
/// confirmed, even when the counts stayed the same.
///
/// ```
/// use yien_proto::{WorktreeFiles, WorktreeWork};
/// let seen = WorktreeWork {
///     changed: 1,
///     ignored: 1,
///     files: WorktreeFiles::of(["changed M a.txt", "ignored .env"]),
///     ..Default::default()
/// };
/// let now = WorktreeWork {
///     ignored: 1,
///     files: WorktreeFiles::of(["ignored .env"]),
///     ..Default::default()
/// };
/// assert!(now.any());
/// assert!(now.within(&seen));
/// assert!(!seen.within(&now));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorktreeWork {
    /// Changed and untracked files, each counted.
    pub changed: u32,
    /// Ignored files and folders; every removal deletes them.
    pub ignored: u32,
    /// Commits not pushed, or on no other branch.
    pub unpushed: u32,
    /// HEAD is detached, so unpushed commits are lost on removal.
    pub detached: bool,
    /// The changed files with their status, the ignored entries and, when
    /// detached commits would be lost, the commit.
    pub files: WorktreeFiles,
}

impl WorktreeWork {
    /// Whether there is anything at all.
    pub fn any(&self) -> bool {
        self.changed > 0 || self.ignored > 0 || self.unpushed > 0
    }

    /// Whether this holds nothing `other` did not: no count is higher, the
    /// head is as detached as it was, and every file is one `other` had.
    pub fn within(&self, other: &WorktreeWork) -> bool {
        self.changed <= other.changed
            && self.ignored <= other.ignored
            && self.unpushed <= other.unpushed
            && self.detached == other.detached
            && self.files.within(&other.files)
    }
}

/// A fingerprint of the entries a removal would delete or strand: one hash
/// per entry, or, past [`WorktreeFiles::MAX_EACH`] entries, one hash of
/// them all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeFiles {
    /// A sorted hash per entry.
    Each(Vec<u64>),
    /// One hash of every entry, for a set too big to send entry by entry.
    Whole(u64),
}

impl Default for WorktreeFiles {
    fn default() -> Self {
        Self::Each(Vec::new())
    }
}

impl WorktreeFiles {
    /// How many entries are fingerprinted one by one.
    pub const MAX_EACH: usize = 4096;

    /// The fingerprint of `entries`, in any order.
    pub fn of<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut hashes: Vec<u64> = entries
            .into_iter()
            .map(|e| fnv1a(e.as_ref().as_bytes()))
            .collect();
        hashes.sort_unstable();
        hashes.dedup();
        if hashes.len() <= Self::MAX_EACH {
            return Self::Each(hashes);
        }
        let bytes: Vec<u8> = hashes.iter().flat_map(|h| h.to_le_bytes()).collect();
        Self::Whole(fnv1a(&bytes))
    }

    /// Whether every entry here is one of `other`'s. A whole-set hash only
    /// matches the same whole set; it never vouches for a part of it.
    pub fn within(&self, other: &WorktreeFiles) -> bool {
        match (self, other) {
            (Self::Each(mine), Self::Each(theirs)) => {
                mine.iter().all(|h| theirs.binary_search(h).is_ok())
            }
            (Self::Whole(mine), Self::Whole(theirs)) => mine == theirs,
            _ => false,
        }
    }
}

/// 64-bit FNV-1a: stable across processes and builds, unlike `std`'s hasher.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// What removing a worktree would lose, as [`CoreMsg::WorktreeChecked`]
/// reports it (RN-08).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeCheck {
    /// git looked.
    Checked {
        /// The counts.
        work: WorktreeWork,
        /// A few of the ignored entries, to name them.
        ignored_examples: Vec<String>,
    },
    /// git could not tell; the reason says why.
    Failed {
        /// For the user.
        reason: String,
    },
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
    /// The id of the profile it runs: `claude`, `shell`.
    pub profile_id: ProfileId,
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
    /// The agent's session id, once known (RF-PAN-20, RF-SES-01).
    pub session_id: Option<String>,
    /// While `state` is "needs you": when it entered that state, as a
    /// sequence number shared by all panes. Lower has waited longer
    /// (RF-ATN-05). Not a time.
    pub needs_you_since: Option<u64>,
    /// While `state` is "needs you", error or done: when it entered that
    /// kind of state, on the same sequence as `needs_you_since`. Lower got
    /// there earlier (RF-ATN-13, RF-ATN-14). Not a time.
    pub attention_since: Option<u64>,
    /// While `state` is "needs you": what the agent asks, when its hooks
    /// said (RF-ATN-13).
    pub question: Option<PendingQuestion>,
    /// The agent's structured channel is unavailable or failed, so its state
    /// cannot be told and stays idle (RN-02): the "signal lost" mark.
    pub signal_lost: bool,
    /// The pane's own program is starting or running: not exited, and not
    /// a stand-in shell after a failed resume. Only then does `session_id`
    /// name a session that runs here (RF-SES-07).
    pub runs_session: bool,
}

/// An agent profile a pane can be started with (RF-AGE-01, RF-PAN-18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileInfo {
    /// Its id, for [`Command::SplitPane`] and [`Command::NewTab`].
    pub id: ProfileId,
    /// The name to show: `Claude Code`, `Shell`.
    pub name: String,
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
    /// The profiles new panes can be started with, the shell first.
    pub profiles: Vec<ProfileInfo>,
    /// Every toast and taskbar flash is muted (RF-ATN-11).
    pub notifications_muted: bool,
    /// The side bars as the session saved them (RF-NAV-10).
    pub side_bars: SideBars,
    /// Pixels the user zoomed the terminals' font by, over
    /// `terminal.font.size`, as the session saved it (RF-CFG-02). The UI
    /// clamps the zoomed size.
    pub terminal_zoom: i32,
}

/// The tab bar and the context bar as the user left them (RF-NAV-10).
///
/// ```
/// let bars = yien_proto::SideBars::default();
/// assert!(!bars.tab_bar.hidden && bars.context_bar.width.is_none() && !bars.review);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct SideBars {
    /// The tab bar on the left.
    pub tab_bar: SideBar,
    /// The context bar on the right.
    pub context_bar: SideBar,
    /// The context bar is widened to review changes.
    pub review: bool,
}

/// One side bar.
///
/// ```
/// let bar = yien_proto::SideBar { hidden: true, width: Some(320.0) };
/// assert!(bar.hidden);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct SideBar {
    /// The user hid the bar.
    pub hidden: bool,
    /// The width the user dragged it to, in logical pixels; `None` is the
    /// default width. The UI clamps it to the window.
    pub width: Option<f32>,
}

/// Everything the core tells the UI, in order.
///
/// While the UI is behind, details of a pane reported between two steps of
/// its program's lifecycle are merged: only the latest state, title,
/// directory and session id are sent. Frames do not travel as events:
/// [`CoreMsg::PaneFrameReady`] says a pane has a new frame, and the UI takes
/// the latest one from the core handle. Events for a pane or tab the UI no
/// longer knows are stale and ignored.
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
    /// A project was removed from the sidebar. Its tabs were closed first,
    /// each with its own [`CoreMsg::TabClosed`].
    ProjectClosed {
        /// The project.
        project: ProjectId,
    },
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
    /// A pane has a new frame to draw (ADR-06: the UI holds visible rows,
    /// never the grid). Take it with the core handle's `take_frame`. Frames
    /// are "last one wins" (plan §4.2): until the UI takes one, newer frames
    /// replace it and no further notice is sent.
    PaneFrameReady {
        /// The pane.
        pane: PaneId,
    },
    /// The agent state of a pane changed (RF-PAN-12). A delta; the pane's
    /// next [`PaneInfo`] carries the same state.
    PaneState {
        /// The pane.
        pane: PaneId,
        /// The new state.
        state: AgentState,
    },
    /// The program of a pane exited; the pane stays and can be relaunched
    /// (RF-PAN-19). A [`CoreMsg::PaneUpdated`] with the exit code follows.
    PaneExited {
        /// The pane.
        pane: PaneId,
        /// The exit code, when the OS reported one.
        code: Option<u32>,
    },
    /// Git facts about a tab's folder changed (RF-ESP-04, RF-CTX-04). Sent
    /// for the selected tab as it changes, and once for each other tab.
    TabGit {
        /// The tab.
        tab: TabId,
        /// What git says.
        git: TabGit,
    },
    /// Toasts and taskbar flashes were muted or unmuted for every tab
    /// (RF-ATN-11).
    NotificationsMuted {
        /// Whether they are off.
        muted: bool,
    },
    /// The answer to [`Command::CheckWorktree`].
    WorktreeChecked {
        /// The tab.
        tab: TabId,
        /// The worktree folder.
        worktree: String,
        /// What git found.
        check: WorktreeCheck,
    },
    /// The answer to [`Command::ListAgentSessions`]: the project's past
    /// sessions, newest first.
    AgentSessions {
        /// The project.
        project: ProjectId,
        /// The request this answers.
        request: u64,
        /// Its sessions; empty when it has none or they could not be read.
        sessions: Vec<AgentSession>,
    },
    /// Something the user asked for finished; the message says what.
    Notice {
        /// For the user.
        message: String,
    },
    /// The answer to [`ClientMsg::CopySelection`].
    SelectionText {
        /// The pane.
        pane: PaneId,
        /// The selected text, or `None` when nothing is selected.
        text: Option<String>,
    },
    /// A program in the pane asked to write the system clipboard (OSC 52).
    /// Take the text with the core handle's `take_clipboard`: it is at most
    /// 1 MiB, the latest write wins, and no further notice is sent until it
    /// is taken. Reads are never offered. Never log the text.
    ClipboardReady {
        /// The pane.
        pane: PaneId,
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
    use crate::frame::{Cell, CellFlags, Frame, Modes, MouseTracking, Rgb, Size};

    #[test]
    fn rn_08_other_files_with_the_same_counts_are_not_within() {
        let seen = WorktreeFiles::of(["changed M a.txt", "changed ? b.txt"]);
        let swapped = WorktreeFiles::of(["changed ? b.txt", "changed ? c.txt"]);
        assert!(!swapped.within(&seen));
        assert!(WorktreeFiles::of(["changed ? b.txt"]).within(&seen));
        assert!(WorktreeFiles::default().within(&seen));
        // The status is part of the entry.
        assert!(!WorktreeFiles::of(["changed D a.txt"]).within(&seen));
    }

    #[test]
    fn rn_08_a_whole_set_hash_vouches_only_for_the_same_set() {
        let many: Vec<String> = (0..=WorktreeFiles::MAX_EACH)
            .map(|i| format!("changed ? f{i}"))
            .collect();
        let whole = WorktreeFiles::of(&many);
        assert!(matches!(whole, WorktreeFiles::Whole(_)));
        assert!(whole.within(&WorktreeFiles::of(many.iter().rev())));
        assert!(!WorktreeFiles::of(&many[1..]).within(&whole));
        assert!(!whole.within(&WorktreeFiles::of(&many[1..])));
    }

    #[test]
    fn rf_atn_13_a_pane_carries_its_question_and_its_place_across_the_wire() {
        let info = PaneInfo {
            id: PaneId::new(3),
            tab: TabId::new(2),
            name: "claude".into(),
            profile: "Claude Code".into(),
            profile_id: ProfileId::new("claude"),
            title: None,
            cwd: None,
            state: AgentState::NeedsYou(NeedsReason::Permission),
            running: true,
            exit_code: None,
            session_id: None,
            needs_you_since: Some(4),
            attention_since: Some(4),
            question: Some(PendingQuestion::new("Permission to use Bash")),
            signal_lost: false,
            runs_session: true,
        };
        let msg = CoreMsg::PaneUpdated(info);
        assert_eq!(round_trip(&msg), msg);
        let json = serde_json::to_string(&PendingQuestion::new("q")).unwrap();
        assert_eq!(json, "\"q\"", "a plain string on the wire");
        let done = PaneInfo {
            state: AgentState::Done,
            needs_you_since: None,
            attention_since: Some(9),
            question: None,
            ..match msg {
                CoreMsg::PaneUpdated(info) => info,
                _ => unreachable!(),
            }
        };
        let msg = CoreMsg::PaneUpdated(done);
        assert_eq!(round_trip(&msg), msg);
    }

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
            Command::NewTab {
                project: ProjectId::new(1),
                name: None,
                profile: Some(ProfileId::new("claude")),
            },
            Command::SplitPane {
                pane: PaneId::new(3),
                orientation: Orientation::Vertical,
                profile: Some(ProfileId::new("claude")),
                before: false,
            },
            Command::SplitPane {
                pane: PaneId::new(3),
                orientation: Orientation::Horizontal,
                profile: None,
                before: true,
            },
            Command::ResizeSplit {
                pane: PaneId::new(3),
                delta: -0.25,
            },
            Command::SetDivider {
                before: PaneId::new(3),
                after: PaneId::new(4),
                share: 0.375,
            },
            Command::CloseProject {
                project: ProjectId::new(1),
            },
            Command::SetProjectCollapsed {
                project: ProjectId::new(1),
                collapsed: true,
            },
            Command::RenameTab {
                tab: TabId::new(2),
                name: "review".into(),
            },
            Command::MoveTab {
                tab: TabId::new(2),
                project: ProjectId::new(1),
                to_index: 3,
            },
            Command::SetTabMuted {
                tab: TabId::new(2),
                muted: true,
            },
            Command::SetNotificationsMuted { muted: true },
            Command::SetSideBars {
                bars: SideBars {
                    tab_bar: SideBar {
                        hidden: true,
                        width: None,
                    },
                    context_bar: SideBar {
                        hidden: false,
                        width: Some(512.5),
                    },
                    review: true,
                },
            },
            Command::SetTerminalZoom { zoom: -3 },
            Command::NewWorktreeTab {
                project: ProjectId::new(1),
                branch: "feat/x".into(),
                base: Some("main".into()),
            },
            Command::NewWorktreeTab {
                project: ProjectId::new(1),
                branch: "fix/y".into(),
                base: None,
            },
            Command::CheckWorktree { tab: TabId::new(2) },
            Command::RemoveWorktreeTab {
                tab: TabId::new(2),
                discard_changes: true,
                confirmed: WorktreeWork {
                    changed: 2,
                    ignored: 1,
                    unpushed: 0,
                    detached: false,
                    files: WorktreeFiles::of(["changed M a.txt", "ignored .env"]),
                },
            },
            Command::MovePane {
                pane: PaneId::new(3),
                target: PaneId::new(4),
                zone: DropZone::Top,
            },
            Command::ListAgentSessions {
                project: ProjectId::new(1),
                request: 7,
            },
            Command::ResumeAgentSession {
                project: ProjectId::new(1),
                profile: ProfileId::new("claude"),
                session_id: "4f1c2a3b-0000-4000-8000-0123456789ab".into(),
            },
        ];
        for command in commands {
            assert_eq!(round_trip(&command), command);
        }
    }

    #[test]
    fn a_split_without_a_side_puts_the_new_pane_after() {
        let split = Command::SplitPane {
            pane: PaneId::new(3),
            orientation: Orientation::Vertical,
            profile: None,
            before: false,
        };
        let json = serde_json::to_string(&split).unwrap();
        let older = json.replace(",\"before\":false", "");
        assert_ne!(older, json);
        assert_eq!(serde_json::from_str::<Command>(&older).unwrap(), split);
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
            ClientMsg::SetTerminalPalette {
                palette: TerminalPalette::Washi,
            },
            ClientMsg::SetShellIntegration { on: false },
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
            muted: true,
            worktree: Some("C:/Code/yien-wt/feat-x".into()),
        };
        assert_eq!(round_trip(&frame), frame);
        let msgs = [
            CoreMsg::PaneFrameReady {
                pane: PaneId::new(3),
            },
            CoreMsg::TabUpdated(tab),
            CoreMsg::ProjectUpdated(ProjectInfo {
                id: ProjectId::new(1),
                name: "yien".into(),
                root: "C:/Code/yien".into(),
                tabs: vec![TabId::new(2)],
                collapsed: true,
            }),
            CoreMsg::ProjectClosed {
                project: ProjectId::new(1),
            },
            CoreMsg::PaneUpdated(PaneInfo {
                id: PaneId::new(3),
                tab: TabId::new(2),
                name: "claude".into(),
                profile: "Claude Code".into(),
                profile_id: ProfileId::new("claude"),
                title: None,
                cwd: Some("C:/Code/yien".into()),
                state: AgentState::Working,
                running: true,
                exit_code: None,
                session_id: Some("4f1c".into()),
                needs_you_since: None,
                attention_since: None,
                question: None,
                signal_lost: false,
                runs_session: true,
            }),
            CoreMsg::NotificationsMuted { muted: true },
            CoreMsg::WorktreeChecked {
                tab: TabId::new(2),
                worktree: "C:/Code/yien-wt/feat-x".into(),
                check: WorktreeCheck::Checked {
                    work: WorktreeWork {
                        changed: 3,
                        ignored: 2,
                        unpushed: 1,
                        detached: true,
                        files: WorktreeFiles::Whole(7),
                    },
                    ignored_examples: vec![".env".into(), "node_modules/".into()],
                },
            },
            CoreMsg::WorktreeChecked {
                tab: TabId::new(2),
                worktree: "C:/Code/yien-wt/feat-x".into(),
                check: WorktreeCheck::Failed {
                    reason: "git was not found".into(),
                },
            },
            CoreMsg::Notice {
                message: "Removed the worktree.".into(),
            },
            CoreMsg::ClipboardReady {
                pane: PaneId::new(3),
            },
            CoreMsg::AgentSessions {
                project: ProjectId::new(1),
                request: 7,
                sessions: vec![
                    AgentSession {
                        profile: ProfileId::new("claude"),
                        id: "4f1c2a3b-0000-4000-8000-0123456789ab".into(),
                        title: Some(SessionTitle::new("fix the build")),
                        modified_ms: 1_762_500_000_000,
                    },
                    AgentSession {
                        profile: ProfileId::new("claude"),
                        id: "5f1c2a3b-0000-4000-8000-0123456789ab".into(),
                        title: None,
                        modified_ms: 0,
                    },
                ],
            },
            CoreMsg::AgentSessions {
                project: ProjectId::new(2),
                request: u64::MAX,
                sessions: vec![],
            },
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
    fn rf_ctx_04_git_facts_round_trip() {
        use crate::{GitFacts, GitFile, GitFileStatus, GitHead, GitUpstream};

        let facts = GitFacts {
            head: GitHead::Branch {
                name: "feat/x".into(),
                unborn: false,
            },
            upstream: Some(GitUpstream {
                name: "origin/feat/x".into(),
                ahead: Some(2),
                behind: Some(1),
            }),
            root: "C:/Code/yien-wt/x".into(),
            linked_worktree: true,
            main_worktree: Some("C:/Code/yien".into()),
            files: vec![
                GitFile {
                    path: "src/new.rs".into(),
                    status: GitFileStatus::Renamed {
                        from: "src/old.rs".into(),
                    },
                },
                GitFile {
                    path: "notes.txt".into(),
                    status: GitFileStatus::Untracked,
                },
            ],
            changed: 2,
        };
        let msgs = [
            CoreMsg::TabGit {
                tab: TabId::new(2),
                git: TabGit::Repository(facts),
            },
            CoreMsg::TabGit {
                tab: TabId::new(2),
                git: TabGit::Repository(GitFacts {
                    head: GitHead::Detached {
                        commit: "0123456".into(),
                    },
                    upstream: None,
                    root: "/srv/app".into(),
                    linked_worktree: false,
                    main_worktree: None,
                    files: vec![],
                    changed: 0,
                }),
            },
            CoreMsg::TabGit {
                tab: TabId::new(3),
                git: TabGit::NoRepository,
            },
            CoreMsg::TabGit {
                tab: TabId::new(3),
                git: TabGit::Unavailable {
                    reason: "git was not found".into(),
                },
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
    fn rf_ses_07_a_listing_never_shows_a_title_in_debug() {
        let msg = CoreMsg::AgentSessions {
            project: ProjectId::new(1),
            request: 1,
            sessions: vec![AgentSession {
                profile: ProfileId::new("claude"),
                id: "4f1c2a3b-0000-4000-8000-0123456789ab".into(),
                title: Some(SessionTitle::new("secret plan")),
                modified_ms: 1,
            }],
        };
        let shown = format!("{msg:?}");
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("SessionTitle(11 chars)"), "{shown}");
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
