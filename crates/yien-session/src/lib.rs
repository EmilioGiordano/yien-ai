//! The YIEN saved session: the model the core persists (projects, tabs, pane trees, panes
//! with their profile, directory, agent session and last command), its invariants, its
//! versioned JSON encoding with migrations, the five generations on disk, and the resume
//! policy for each restored pane.
//!
//! No OS calls: files go through the [`Store`] trait. [`MemStore`] serves tests; the `fs`
//! feature adds `FsStore`, built only on `std::fs`.
//!
//! ```
//! use yien_proto::{PaneId, ProjectId, TabId};
//! use yien_session::*;
//!
//! let pane = PaneId::new(1);
//! let snapshot = SessionSnapshot::new(1_700_000_000_000, vec![ProjectRecord {
//!     id: ProjectId::new(1),
//!     name: "yien".into(),
//!     root: "C:/code/yien".into(),
//!     setup_script: None,
//!     tabs: vec![TabRecord {
//!         id: TabId::new(1),
//!         title: "main".into(),
//!         dir: "C:/code/yien".into(),
//!         layout: LayoutRecord::Leaf { pane },
//!         panes: vec![PaneRecord {
//!             id: pane,
//!             profile: ProfileId::new("claude"),
//!             cwd: "C:/code/yien".into(),
//!             agent: Some(AgentRecord {
//!                 kind: ProfileId::new("claude"),
//!                 session_id: Some(SessionRef::new("4f1c")),
//!                 ended_explicitly: false,
//!             }),
//!             last_command: None,
//!             name: None,
//!         }],
//!         focused: pane,
//!         branch: None,
//!         worktree: None,
//!         archived: false,
//!     }],
//! }]);
//!
//! let mut store = MemStore::new();
//! save(&mut store, &snapshot).unwrap();
//! let restored = load(&store).restored.unwrap().snapshot;
//! assert_eq!(restored, snapshot);
//! let first = &restored.projects[0].tabs[0].panes[0];
//! assert_eq!(
//!     resume_plan(first),
//!     ResumeAction::Resume { kind: ProfileId::new("claude"), session: SessionRef::new("4f1c") }
//! );
//! ```

#![forbid(unsafe_code)]

mod codec;
mod generations;
mod model;
mod restore;
mod store;
mod validate;

pub use codec::{
    CodecError, MIGRATIONS, Migration, SCHEMA_VERSION, deserialize, deserialize_with, migrate,
    migrate_with, schema_version_of, serialize,
};
pub use generations::{
    BACKUPS, CURRENT_FILE, DAMAGED_FILE, LoadReport, ROTATION_INTERVAL_MS, Restored, SaveError,
    Saved, SkipReason, Skipped, generation_name, load, preserve_newer, save,
};
pub use model::{
    AgentRecord, LayoutRecord, Orientation, PaneRecord, ProfileId, ProjectRecord, Selection,
    SessionRef, SessionSnapshot, TabRecord,
};
pub use restore::{ResumeAction, missing_dirs, resume_plan};
#[cfg(feature = "fs")]
pub use store::FsStore;
pub use store::{MemStore, Store, StoreError};
pub use validate::{Problem, RATIO_EPSILON, validate};
