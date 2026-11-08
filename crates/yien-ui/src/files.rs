//! What the binary hands the window for the file tree (RF-CTX-01,
//! RF-CTX-03): listing a folder, searching by name and opening a file, all
//! on the platform layer and off the UI thread.

use std::path::PathBuf;

use yien_proto::files::{DirListing, SearchResult};

/// A folder's listing, or why it failed.
pub type ListDone = Box<dyn FnOnce(Result<DirListing, String>) + Send>;

/// A search's answer.
pub type SearchDone = Box<dyn FnOnce(SearchResult) + Send>;

/// What went wrong opening a file, if anything.
pub type OpenDone = Box<dyn FnOnce(Option<String>) + Send>;

/// Lists, searches and opens files for the window. Every call returns at
/// once; its callback runs later, from any thread.
///
/// ```
/// use std::path::PathBuf;
/// use yien_ui::{FilesHost, ListDone, OpenDone, SearchDone};
///
/// struct Nothing;
/// impl FilesHost for Nothing {
///     fn list(&self, _root: PathBuf, _dir: String, done: ListDone) {
///         done(Err("no files here".into()));
///     }
///     fn search(&self, _root: PathBuf, _query: String, done: SearchDone) {
///         done(yien_proto::files::SearchResult {
///             hits: vec![],
///             end: yien_proto::files::SearchEnd::Complete,
///         });
///     }
///     fn open(&self, _file: PathBuf, _editor: Option<String>, done: OpenDone) {
///         done(Some("no editor here".into()));
///     }
/// }
/// ```
pub trait FilesHost {
    /// Lists one level of the folder `dir` (relative, parts joined with
    /// `/`, empty for the root) under `root`.
    fn list(&self, root: PathBuf, dir: String, done: ListDone);
    /// Searches `root` by name; a newer search may cancel this one.
    fn search(&self, root: PathBuf, query: String, done: SearchDone);
    /// Opens `file` in `editor` (a command line), or in the OS default app
    /// when `None`.
    fn open(&self, file: PathBuf, editor: Option<String>, done: OpenDone);
}

/// The file tree's host, and a test-only start state.
pub struct Files {
    /// Lists, searches and opens.
    pub host: Box<dyn FilesHost>,
    /// Folders (relative, parts joined with `/`) to expand in each tab's
    /// tree when it is first shown; when any are given, the context bar
    /// starts on its Files view. Empty outside window checks.
    pub seed: Vec<String>,
}
