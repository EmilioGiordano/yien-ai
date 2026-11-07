//! The saved session: what the core writes so a restart restores every tab.

use serde::{Deserialize, Serialize};
use yien_proto::{PaneId, ProjectId, TabId};

use crate::codec::SCHEMA_VERSION;

/// The name of a profile (`claude`, `codex`, `pwsh`), as its TOML file declares it.
///
/// ```
/// let profile = yien_session::ProfileId::new("claude");
/// assert_eq!(profile.as_str(), "claude");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(String);

impl ProfileId {
    /// Wraps a profile name.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The profile name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The exact id an agent CLI gave its session, opaque to YIEN (RN-05).
///
/// ```
/// let session = yien_session::SessionRef::new("4f1c-9a");
/// assert_eq!(session.as_str(), "4f1c-9a");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionRef(String);

impl SessionRef {
    /// Wraps a session id as the CLI reported it.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The session id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Everything the core persists, as one JSON document (ADR-10).
///
/// ```
/// let snapshot = yien_session::SessionSnapshot::new(1_700_000_000_000, Vec::new());
/// assert_eq!(snapshot.schema_version, yien_session::SCHEMA_VERSION);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// The schema this document follows; always [`SCHEMA_VERSION`] when written.
    pub schema_version: u32,
    /// When the caller took the snapshot, in milliseconds since the Unix epoch.
    pub saved_at: u64,
    /// The project and tab that were selected.
    #[serde(default)]
    pub selected: Selection,
    /// The projects in sidebar order.
    pub projects: Vec<ProjectRecord>,
}

impl SessionSnapshot {
    /// A snapshot in the current schema.
    ///
    /// ```
    /// let snapshot = yien_session::SessionSnapshot::new(5, Vec::new());
    /// assert_eq!(snapshot.saved_at, 5);
    /// ```
    pub fn new(saved_at: u64, projects: Vec<ProjectRecord>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            saved_at,
            selected: Selection::default(),
            projects,
        }
    }
}

/// The project and tab selected when the snapshot was taken; each `None` when nothing was.
///
/// ```
/// let selected = yien_session::Selection::default();
/// assert!(selected.project.is_none() && selected.tab.is_none());
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// The selected project.
    #[serde(default)]
    pub project: Option<ProjectId>,
    /// The selected tab; it belongs to `project` when both are set.
    #[serde(default)]
    pub tab: Option<TabId>,
}

/// One project in the sidebar.
///
/// ```
/// let project = yien_session::ProjectRecord {
///     id: yien_proto::ProjectId::new(1),
///     name: "yien".into(),
///     root: "C:/code/yien".into(),
///     setup_script: None,
///     collapsed: false,
///     tabs: Vec::new(),
/// };
/// assert!(project.tabs.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRecord {
    /// The project's id.
    pub id: ProjectId,
    /// The name shown in the sidebar.
    pub name: String,
    /// The project's root folder.
    pub root: String,
    /// The script run to prepare a new worktree of the project.
    #[serde(default)]
    pub setup_script: Option<String>,
    /// Whether the project's tabs are folded away in the sidebar (RF-ESP-17).
    #[serde(default)]
    pub collapsed: bool,
    /// The project's tabs, in order.
    pub tabs: Vec<TabRecord>,
}

/// One tab and its pane tree.
///
/// ```
/// use yien_proto::{PaneId, TabId};
/// use yien_session::{LayoutRecord, PaneRecord, ProfileId, TabRecord};
///
/// let pane = PaneId::new(1);
/// let tab = TabRecord {
///     id: TabId::new(1),
///     title: "Tab 1".into(),
///     name: Some("main".into()),
///     dir: "C:/code/yien".into(),
///     layout: LayoutRecord::Leaf { pane },
///     panes: vec![PaneRecord {
///         id: pane,
///         profile: ProfileId::new("pwsh"),
///         cwd: "C:/code/yien".into(),
///         agent: None,
///         last_command: None,
///         name: None,
///     }],
///     focused: pane,
///     branch: None,
///     worktree: None,
///     archived: false,
/// };
/// assert_eq!(tab.layout.panes(), vec![pane]);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabRecord {
    /// The tab's id.
    pub id: TabId,
    /// The automatic title, shown when the user has not named the tab.
    pub title: String,
    /// The name the user gave the tab, shown instead of `title` (RF-ESP-16).
    #[serde(default)]
    pub name: Option<String>,
    /// The tab's directory, checked on restore (RF-SES-12).
    pub dir: String,
    /// The git branch the tab works on.
    #[serde(default)]
    pub branch: Option<String>,
    /// The worktree folder, when the tab has its own.
    #[serde(default)]
    pub worktree: Option<String>,
    /// Whether the tab is archived rather than active.
    #[serde(default)]
    pub archived: bool,
    /// How the panes are arranged.
    pub layout: LayoutRecord,
    /// Every pane of the tab; exactly the leaves of `layout`.
    pub panes: Vec<PaneRecord>,
    /// The pane that had focus.
    pub focused: PaneId,
}

/// One terminal pane.
///
/// ```
/// let pane = yien_session::PaneRecord {
///     id: yien_proto::PaneId::new(4),
///     profile: yien_session::ProfileId::new("pwsh"),
///     cwd: "C:/code".into(),
///     agent: None,
///     last_command: Some("cargo test".into()),
///     name: None,
/// };
/// assert!(pane.agent.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneRecord {
    /// The pane's id, unique in the whole snapshot.
    pub id: PaneId,
    /// The name the user gave the pane, if any.
    #[serde(default)]
    pub name: Option<String>,
    /// The profile the pane runs.
    pub profile: ProfileId,
    /// The pane's working directory.
    pub cwd: String,
    /// The agent session the pane holds, if it runs an agent.
    pub agent: Option<AgentRecord>,
    /// The last command typed in a shell pane, offered for rerun on restore (RF-SES-05).
    pub last_command: Option<String>,
}

/// The agent session a pane holds.
///
/// ```
/// let agent = yien_session::AgentRecord {
///     kind: yien_session::ProfileId::new("claude"),
///     session_id: Some(yien_session::SessionRef::new("4f1c")),
///     ended_explicitly: false,
/// };
/// assert!(!agent.ended_explicitly);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRecord {
    /// The agent profile that started the session.
    pub kind: ProfileId,
    /// The session id from the last `SessionStart`; `None` until one arrives.
    pub session_id: Option<SessionRef>,
    /// Whether the user ended the session; such a session is never resumed (RN-04).
    pub ended_explicitly: bool,
}

/// Which way a split lays out its children. Mirrors `yien_layout::Orientation`.
///
/// ```
/// assert_ne!(yien_session::Orientation::Horizontal, yien_session::Orientation::Vertical);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    /// Children side by side, left to right.
    Horizontal,
    /// Children stacked, top to bottom.
    Vertical,
}

/// A tab's pane tree as stored; the core converts it to and from `yien_layout::Tree`.
///
/// ```
/// use yien_proto::PaneId;
/// use yien_session::{LayoutRecord, Orientation};
///
/// let layout = LayoutRecord::Split {
///     orientation: Orientation::Horizontal,
///     children: vec![
///         LayoutRecord::Leaf { pane: PaneId::new(1) },
///         LayoutRecord::Leaf { pane: PaneId::new(2) },
///     ],
///     ratios: vec![0.5, 0.5],
/// };
/// assert_eq!(layout.panes(), vec![PaneId::new(1), PaneId::new(2)]);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutRecord {
    /// One pane filling its area.
    Leaf {
        /// The pane shown here.
        pane: PaneId,
    },
    /// Children sharing the area along one axis.
    Split {
        /// The axis the children share.
        orientation: Orientation,
        /// The children, left to right or top to bottom.
        children: Vec<LayoutRecord>,
        /// Each child's share of the area, one per child, summing to 1.
        ratios: Vec<f32>,
    },
}

impl LayoutRecord {
    /// Every leaf's pane, in tree order, repeats included.
    ///
    /// ```
    /// let leaf = yien_session::LayoutRecord::Leaf { pane: yien_proto::PaneId::new(3) };
    /// assert_eq!(leaf.panes(), vec![yien_proto::PaneId::new(3)]);
    /// ```
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_panes(&mut out);
        out
    }

    fn collect_panes(&self, out: &mut Vec<PaneId>) {
        match self {
            LayoutRecord::Leaf { pane } => out.push(*pane),
            LayoutRecord::Split { children, .. } => {
                for child in children {
                    child.collect_panes(out);
                }
            }
        }
    }
}
