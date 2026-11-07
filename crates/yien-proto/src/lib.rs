//! The messages the YIEN core and UI exchange, and the ids they refer to.
//!
//! The UI never calls the core directly: it sends commands and receives events
//! defined here, so the same core can run in process or as a separate host.
//!
//! ```
//! use yien_proto::{PaneId, PROTO_VERSION};
//!
//! assert_eq!(PROTO_VERSION, 1);
//! assert_eq!(PaneId::new(7).get(), 7);
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Version of the wire protocol. Bumped on any incompatible change to a message.
///
/// ```
/// assert!(yien_proto::PROTO_VERSION >= 1);
/// ```
pub const PROTO_VERSION: u32 = 1;

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

/// The command catalog (ADR-19): every action the palette, the keymap and the
/// future CLI can trigger. Empty until the first commands land in M2.
///
/// ```
/// fn handle(command: yien_proto::Command) {
///     match command {}
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {}

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
    fn empty_catalog_rejects_any_command() {
        assert!(serde_json::from_str::<Command>("\"Anything\"").is_err());
    }
}
