//! The messages the YIEN core and UI exchange, and the ids they refer to.
//!
//! The UI never calls the core directly: it sends commands and receives events
//! defined here, so the same core can run in process or as a separate host.
//!
//! ```
//! use yien_proto::{ClientMsg, Command, Orientation, PaneId, PROTO_VERSION};
//!
//! assert_eq!(PROTO_VERSION, 11);
//! let split = ClientMsg::Command(Command::SplitPane {
//!     pane: PaneId::new(7),
//!     orientation: Orientation::Horizontal,
//!     profile: None,
//! });
//! assert!(matches!(split, ClientMsg::Command(_)));
//! ```

#![forbid(unsafe_code)]

pub mod files;
pub mod frame;
mod git;
pub mod hook_frame;
mod msg;
mod path;

use serde::{Deserialize, Serialize};

pub use git::{GitFacts, GitFile, GitFileStatus, GitHead, GitUpstream, MAX_GIT_FILES, TabGit};
pub use msg::{
    AgentSession, AgentState, ClientMsg, Command, CoreMsg, Direction, DropZone, ErrorReason,
    InputBytes, LayoutChild, LayoutNode, NeedsReason, Orientation, PaneInfo, PendingQuestion,
    ProfileId, ProfileInfo, ProjectInfo, SessionTitle, TabInfo, TerminalPalette, WorkspaceSnapshot,
    WorktreeCheck, WorktreeFiles, WorktreeWork,
};
pub use path::{folder_key, same_folder};

/// Version of the wire protocol. Bumped on any incompatible change to a message.
///
/// ```
/// assert!(yien_proto::PROTO_VERSION >= 1);
/// ```
pub const PROTO_VERSION: u32 = 11;

/// The id of the built-in profile that runs the user's shell; every other
/// profile runs an agent.
///
/// ```
/// assert_eq!(yien_proto::ProfileId::new(yien_proto::SHELL_PROFILE).as_str(), "shell");
/// ```
pub const SHELL_PROFILE: &str = "shell";

macro_rules! id_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Wraps a raw id.
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

            /// The raw id.
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

id_newtype! {
    /// Identifies a project in the sidebar.
    ///
    /// ```
    /// let id = yien_proto::ProjectId::new(3);
    /// assert_eq!(id.get(), 3);
    /// ```
    ProjectId
}

id_newtype! {
    /// Identifies a tab inside a project.
    ///
    /// ```
    /// let id = yien_proto::TabId::new(3);
    /// assert_eq!(id.get(), 3);
    /// ```
    TabId
}

id_newtype! {
    /// Identifies a terminal pane inside a tab.
    ///
    /// ```
    /// let id = yien_proto::PaneId::new(3);
    /// assert_eq!(id.get(), 3);
    /// ```
    PaneId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_as_bare_numbers() {
        assert_eq!(serde_json::to_string(&PaneId::new(42)).unwrap(), "42");
        assert_eq!(serde_json::to_string(&TabId::new(0)).unwrap(), "0");
        assert_eq!(
            serde_json::to_string(&ProjectId::new(u64::MAX)).unwrap(),
            u64::MAX.to_string()
        );
    }

    #[test]
    fn ids_round_trip() {
        for raw in [0, 1, 9_000, u64::MAX] {
            let json = serde_json::to_string(&ProjectId::new(raw)).unwrap();
            let back: ProjectId = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ProjectId::new(raw));
        }
    }

    #[test]
    fn ids_reject_non_numbers() {
        assert!(serde_json::from_str::<TabId>("\"7\"").is_err());
        assert!(serde_json::from_str::<TabId>("-1").is_err());
    }

    #[test]
    fn the_catalog_rejects_unknown_commands() {
        assert!(serde_json::from_str::<Command>("\"Anything\"").is_err());
    }
}
