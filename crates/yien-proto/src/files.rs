//! A tab's folder as the file tree sees it (RF-CTX-01): one folder's
//! entries and the matches of a search by name. Plain values; not part of
//! any message yet, so [`crate::PROTO_VERSION`] does not cover them.
//!
//! ```
//! use yien_proto::files::{DirEntry, DirListing, EntryKind};
//!
//! let listing = DirListing {
//!     entries: vec![DirEntry { name: "src".into(), kind: EntryKind::Dir }],
//!     truncated: false,
//! };
//! assert!(listing.entries[0].kind.is_dir());
//! ```

use serde::{Deserialize, Serialize};

/// The most entries one folder listing carries; a bigger folder ends with
/// [`DirListing::truncated`].
pub const MAX_DIR_ENTRIES: usize = 5_000;

/// What an entry is. Links are never followed by a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// A folder.
    Dir,
    /// Anything that is not a folder or a link.
    File,
    /// A symbolic link or a junction; `dir` when it points at a folder.
    Link {
        /// Whether the target is a folder.
        dir: bool,
    },
}

impl EntryKind {
    /// Whether it can be expanded: a folder or a link to one.
    ///
    /// ```
    /// use yien_proto::files::EntryKind;
    /// assert!(EntryKind::Link { dir: true }.is_dir());
    /// assert!(!EntryKind::File.is_dir());
    /// ```
    pub fn is_dir(self) -> bool {
        matches!(self, EntryKind::Dir | EntryKind::Link { dir: true })
    }
}

/// One entry of a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// The file name; a name that is not valid Unicode has replacement
    /// characters.
    pub name: String,
    /// What it is.
    pub kind: EntryKind,
}

/// One folder's entries, in no particular order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirListing {
    /// At most [`MAX_DIR_ENTRIES`].
    pub entries: Vec<DirEntry>,
    /// The folder holds more entries than were listed.
    pub truncated: bool,
}

/// An entry whose name matched a search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Relative to the searched folder, parts joined with `/`.
    pub path: String,
    /// What it is.
    pub kind: EntryKind,
}

/// Why a search stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchEnd {
    /// Every entry within the depth limit was looked at.
    Complete,
    /// It found as many matches as it keeps.
    TooManyHits,
    /// It looked at as many entries as it may.
    TooManyEntries,
    /// It ran out of time.
    TimedOut,
    /// A newer search replaced it.
    Cancelled,
}

/// What a search by name found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// The matches, in walk order.
    pub hits: Vec<SearchHit>,
    /// Why it stopped.
    pub end: SearchEnd,
}
