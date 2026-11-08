//! The file tree of a tab's folder (RF-CTX-01), decided without drawing:
//! which folders are expanded and listed, the rows they flatten to, the
//! keyboard cursor and the search by name. Listings and searches run on the
//! platform layer; this model says what to ask for and takes the answers.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use yien_proto::files::{DirEntry, DirListing, EntryKind, SearchEnd, SearchHit, SearchResult};

use crate::matcher;

/// One drawn row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Row {
    /// A file or folder of the tree.
    Entry {
        /// Relative to the root, parts joined with `/`.
        path: String,
        name: String,
        kind: EntryKind,
        depth: usize,
        expanded: bool,
    },
    /// A folder whose listing has not arrived.
    Loading { depth: usize },
    /// A folder that holds more entries than were listed.
    More { depth: usize },
    /// A folder that could not be listed.
    Failed { depth: usize, reason: String },
    /// A search match.
    Hit { path: String, kind: EntryKind },
}

impl Row {
    /// The entry the row stands for, when it is one.
    pub(crate) fn path(&self) -> Option<&str> {
        match self {
            Row::Entry { path, .. } | Row::Hit { path, .. } => Some(path),
            _ => None,
        }
    }

    fn kind(&self) -> Option<EntryKind> {
        match self {
            Row::Entry { kind, .. } | Row::Hit { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

/// A search to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchRequest {
    pub(crate) id: u64,
    pub(crate) query: String,
}

/// What Enter on the cursor's row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Activation {
    /// Open this file, relative to the root.
    Open(String),
    /// List these folders.
    List(Vec<String>),
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Dir {
    Loading,
    Loaded {
        entries: Vec<DirEntry>,
        truncated: bool,
    },
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Search {
    Idle,
    Running {
        id: u64,
    },
    Done {
        hits: Vec<SearchHit>,
        end: SearchEnd,
    },
    /// Stopped before it finished: another tab's search replaced it.
    Interrupted,
}

/// One tab's tree. The root's listing is asked for by [`FileTree::new`].
#[derive(Debug, Clone)]
pub(crate) struct FileTree {
    root: String,
    /// Listings by folder, `""` for the root.
    dirs: HashMap<String, Dir>,
    expanded: HashSet<String>,
    /// Listings asked for and not answered yet, so a folder is not asked
    /// for twice at once.
    pending: HashSet<String>,
    /// Pending listings a refresh asked for again: their answer may predate
    /// the change, so they are listed once more when it arrives.
    dirty: HashSet<String>,
    tree_cursor: Option<String>,
    hit_cursor: Option<String>,
    query: String,
    search: Search,
    searches: u64,
}

impl FileTree {
    /// A tree of `root`, and the listing to ask for.
    pub(crate) fn new(root: &str) -> (Self, String) {
        let mut dirs = HashMap::new();
        dirs.insert(String::new(), Dir::Loading);
        let tree = Self {
            root: root.to_owned(),
            dirs,
            expanded: HashSet::new(),
            pending: HashSet::from([String::new()]),
            dirty: HashSet::new(),
            tree_cursor: None,
            hit_cursor: None,
            query: String::new(),
            search: Search::Idle,
            searches: 0,
        };
        (tree, String::new())
    }

    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    /// `rel` as a path under the root; `None` when it would leave it.
    pub(crate) fn path_of(&self, rel: &str) -> Option<PathBuf> {
        join_relative(&self.root, rel)
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn filtering(&self) -> bool {
        !self.query.is_empty()
    }

    /// Takes a folder's listing, or why it failed, and returns the listing
    /// to ask for again when a refresh came while this one was on the way.
    /// A folder no one asked for is ignored.
    pub(crate) fn listed(&mut self, dir: &str, listing: Result<DirListing, String>) -> Vec<String> {
        self.pending.remove(dir);
        let again = if self.dirty.remove(dir) && self.dirs.contains_key(dir) {
            self.ask([dir.to_owned()], false)
        } else {
            Vec::new()
        };
        let Some(slot) = self.dirs.get_mut(dir) else {
            return again;
        };
        *slot = match listing {
            Ok(DirListing {
                mut entries,
                truncated,
            }) => {
                sort_entries(&mut entries);
                Dir::Loaded { entries, truncated }
            }
            Err(reason) => {
                // Nothing below a folder that cannot be listed can show.
                self.expanded.retain(|e| !inside(e, dir));
                Dir::Failed(reason)
            }
        };
        again
    }

    /// The listings of `dirs` not already asked for, marked as asked. With
    /// `refresh`, one already on the way is marked to be asked for again.
    fn ask(&mut self, dirs: impl IntoIterator<Item = String>, refresh: bool) -> Vec<String> {
        let mut asked = Vec::new();
        for dir in dirs {
            if self.pending.insert(dir.clone()) {
                asked.push(dir);
            } else if refresh {
                self.dirty.insert(dir);
            }
        }
        asked
    }

    /// Expands a folder; its listing is asked for again unless it already
    /// is, the last one shown meanwhile.
    pub(crate) fn expand(&mut self, dir: &str) -> Vec<String> {
        self.expanded.insert(dir.to_owned());
        self.dirs.entry(dir.to_owned()).or_insert(Dir::Loading);
        self.ask([dir.to_owned()], false)
    }

    /// Collapses a folder and every folder inside it; a cursor inside it
    /// moves onto it.
    pub(crate) fn collapse(&mut self, dir: &str) {
        self.expanded.retain(|e| e != dir && !inside(e, dir));
        if self.tree_cursor.as_deref().is_some_and(|c| inside(c, dir)) {
            self.tree_cursor = Some(dir.to_owned());
        }
    }

    /// Expands every folder on the way to `path` and `path` itself when it
    /// is a folder, and puts the cursor there.
    pub(crate) fn reveal(&mut self, path: &str, is_dir: bool) -> Vec<String> {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let upto = if is_dir {
            parts.len()
        } else {
            parts.len().saturating_sub(1)
        };
        let mut requests = Vec::new();
        for n in 1..=upto {
            requests.extend(self.expand(&parts[..n].join("/")));
        }
        self.tree_cursor = Some(parts.join("/"));
        requests
    }

    /// The listings to ask for again: the root and every expanded folder.
    /// Collapsed folders forget their listing.
    pub(crate) fn refresh(&mut self) -> Vec<String> {
        let expanded = &self.expanded;
        self.dirs
            .retain(|dir, _| dir.is_empty() || expanded.contains(dir));
        let mut dirs: Vec<String> = self.dirs.keys().cloned().collect();
        dirs.sort();
        self.ask(dirs, true)
    }

    /// The rows to draw: the search's matches while filtering, else the
    /// tree, folders first.
    pub(crate) fn rows(&self) -> Vec<Row> {
        if self.filtering() {
            return match &self.search {
                Search::Done { hits, .. } => hits
                    .iter()
                    .map(|h| Row::Hit {
                        path: h.path.clone(),
                        kind: h.kind,
                    })
                    .collect(),
                _ => Vec::new(),
            };
        }
        let mut rows = Vec::new();
        self.flatten("", 0, &mut rows);
        rows
    }

    fn flatten(&self, dir: &str, depth: usize, rows: &mut Vec<Row>) {
        match self.dirs.get(dir) {
            None | Some(Dir::Loading) => rows.push(Row::Loading { depth }),
            Some(Dir::Failed(reason)) => rows.push(Row::Failed {
                depth,
                reason: reason.clone(),
            }),
            Some(Dir::Loaded { entries, truncated }) => {
                for entry in entries {
                    let path = child(dir, &entry.name);
                    let expanded = entry.kind.is_dir() && self.expanded.contains(&path);
                    rows.push(Row::Entry {
                        path: path.clone(),
                        name: entry.name.clone(),
                        kind: entry.kind,
                        depth,
                        expanded,
                    });
                    if expanded {
                        self.flatten(&path, depth + 1, rows);
                    }
                }
                if *truncated {
                    rows.push(Row::More { depth });
                }
            }
        }
    }

    /// Whether the root was listed and holds nothing.
    pub(crate) fn root_is_empty(&self) -> bool {
        matches!(self.dirs.get(""), Some(Dir::Loaded { entries, truncated: false }) if entries.is_empty())
    }

    /// The row the cursor is on.
    pub(crate) fn cursor(&self) -> Option<&str> {
        if self.filtering() {
            self.hit_cursor.as_deref()
        } else {
            self.tree_cursor.as_deref()
        }
    }

    /// Where the cursor's row is in `rows`.
    pub(crate) fn cursor_index(&self, rows: &[Row]) -> Option<usize> {
        let cursor = self.cursor()?;
        rows.iter().position(|r| r.path() == Some(cursor))
    }

    pub(crate) fn set_cursor(&mut self, path: &str) {
        let slot = if self.filtering() {
            &mut self.hit_cursor
        } else {
            &mut self.tree_cursor
        };
        *slot = Some(path.to_owned());
    }

    /// Moves the cursor `by` rows among the entries, stopping at the ends.
    /// A cursor on no drawn row starts at the top.
    pub(crate) fn move_cursor(&mut self, by: isize) {
        let rows = self.rows();
        let entries: Vec<&str> = rows.iter().filter_map(Row::path).collect();
        let Some(last) = entries.len().checked_sub(1) else {
            return;
        };
        let at = self
            .cursor()
            .and_then(|c| entries.iter().position(|e| *e == c));
        let to = match at {
            Some(at) => at.saturating_add_signed(by).min(last),
            None => 0,
        };
        let path = entries[to].to_owned();
        self.set_cursor(&path);
    }

    fn cursor_row(&self) -> Option<Row> {
        let rows = self.rows();
        let at = self.cursor_index(&rows)?;
        rows.into_iter().nth(at)
    }

    /// Left: collapses an expanded folder, else goes to the parent folder.
    pub(crate) fn left(&mut self) {
        if self.filtering() {
            return;
        }
        match self.cursor_row() {
            Some(Row::Entry {
                path,
                expanded: true,
                ..
            }) => self.collapse(&path),
            Some(Row::Entry { path, .. }) => {
                if let Some((parent, _)) = path.rsplit_once('/') {
                    self.tree_cursor = Some(parent.to_owned());
                }
            }
            _ => {}
        }
    }

    /// Right: expands a folder, or goes to the first entry of an expanded
    /// one.
    pub(crate) fn right(&mut self) -> Vec<String> {
        if self.filtering() {
            return Vec::new();
        }
        match self.cursor_row() {
            Some(Row::Entry {
                path,
                kind,
                expanded: false,
                ..
            }) if kind.is_dir() => self.expand(&path),
            Some(Row::Entry { expanded: true, .. }) => {
                self.move_cursor(1);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Enter: opens a file, expands or collapses a folder. On a search
    /// match that is a folder, the search ends and the folder is revealed.
    pub(crate) fn activate(&mut self) -> Activation {
        let Some(row) = self.cursor_row() else {
            return Activation::Nothing;
        };
        self.activate_row(&row)
    }

    /// What a click on `path` does: the cursor goes there, and a folder
    /// expands or collapses. A double click on a file opens it; on a folder
    /// its second click is ignored, or the folder would toggle back.
    pub(crate) fn click(&mut self, path: &str, double: bool) -> Activation {
        let row = self.rows().into_iter().find(|r| r.path() == Some(path));
        let Some(row) = row else {
            return Activation::Nothing;
        };
        self.set_cursor(path);
        match row.kind() {
            Some(kind) if kind.is_dir() && double => Activation::Nothing,
            Some(kind) if kind.is_dir() => self.activate_row(&row),
            Some(_) if double => self.activate_row(&row),
            _ => Activation::Nothing,
        }
    }

    fn activate_row(&mut self, row: &Row) -> Activation {
        match row {
            Row::Entry {
                path,
                kind,
                expanded,
                ..
            } if kind.is_dir() => {
                if *expanded {
                    self.collapse(path);
                    Activation::Nothing
                } else {
                    Activation::List(self.expand(path))
                }
            }
            Row::Entry { path, .. } => Activation::Open(path.clone()),
            Row::Hit { path, kind } if kind.is_dir() => {
                let path = path.clone();
                self.set_query("");
                Activation::List(self.reveal(&path, true))
            }
            Row::Hit { path, .. } => Activation::Open(path.clone()),
            _ => Activation::Nothing,
        }
    }

    /// Takes the filter's text. A new non-empty query starts a search and
    /// drops the last one's answer; an empty one goes back to the tree.
    pub(crate) fn set_query(&mut self, query: &str) -> Option<SearchRequest> {
        let query = query.trim();
        if query == self.query {
            return None;
        }
        self.query = query.to_owned();
        self.hit_cursor = None;
        if query.is_empty() {
            self.search = Search::Idle;
            return None;
        }
        Some(self.new_search())
    }

    fn new_search(&mut self) -> SearchRequest {
        self.searches += 1;
        let id = self.searches;
        self.search = Search::Running { id };
        SearchRequest {
            id,
            query: self.query.clone(),
        }
    }

    /// Whether a search is waiting for its answer.
    pub(crate) fn searching(&self) -> bool {
        matches!(self.search, Search::Running { .. })
    }

    /// Whether the search with this id is the one waiting for its answer.
    pub(crate) fn awaits(&self, id: u64) -> bool {
        self.search == (Search::Running { id })
    }

    /// The search to run again when the tree is shown again: one that was
    /// interrupted, or whose answer may have been lost while hidden.
    pub(crate) fn resume_search(&mut self) -> Option<SearchRequest> {
        let unfinished = matches!(self.search, Search::Running { .. } | Search::Interrupted);
        (self.filtering() && unfinished).then(|| self.new_search())
    }

    /// Takes a search's answer, best matches first; an older search's is
    /// dropped, and a cancelled one leaves the search unfinished.
    pub(crate) fn searched(&mut self, id: u64, result: SearchResult) {
        if !self.awaits(id) {
            return;
        }
        if result.end == SearchEnd::Cancelled {
            self.search = Search::Interrupted;
            return;
        }
        let mut hits = result.hits;
        let query = &self.query;
        hits.sort_by_cached_key(|h| {
            let name = h.path.rsplit('/').next().unwrap_or(&h.path);
            let score = matcher::fuzzy(query, name).map_or(0, |m| m.score);
            (Reverse(score), h.path.len(), h.path.clone())
        });
        self.hit_cursor = hits.first().map(|h| h.path.clone());
        self.search = Search::Done {
            hits,
            end: result.end,
        };
    }

    /// A line about the search under the matches, if any.
    pub(crate) fn search_note(&self) -> Option<String> {
        match &self.search {
            Search::Idle => None,
            Search::Running { .. } => Some("Searching…".to_owned()),
            Search::Interrupted => Some("The search was stopped.".to_owned()),
            Search::Done { hits, end } => match end {
                SearchEnd::Complete if hits.is_empty() => Some(format!(
                    "No file or folder name contains \"{}\".",
                    self.query
                )),
                SearchEnd::Complete | SearchEnd::Cancelled => None,
                SearchEnd::TooManyHits => Some(format!(
                    "The first {} matches. Type more to narrow them.",
                    hits.len()
                )),
                SearchEnd::TooManyEntries => Some(
                    "The search stopped after looking at many entries. Type more to narrow it."
                        .to_owned(),
                ),
                SearchEnd::TimedOut => {
                    Some("The search ran out of time; the matches so far are shown.".to_owned())
                }
                SearchEnd::Unavailable => Some("The file search could not run.".to_owned()),
            },
        }
    }
}

/// `name` inside the folder `dir`.
fn child(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// `rel` (parts joined with `/`) under `root`, or `None` for a part that
/// would leave it: `.`, `..`, and on Windows a `\` or a drive's `:`.
pub(crate) fn join_relative(root: &str, rel: &str) -> Option<PathBuf> {
    let mut path = PathBuf::from(root);
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        let foreign = cfg!(windows) && part.contains(['\\', ':']);
        if part == "." || part == ".." || foreign {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

/// Whether `path` lies below the folder `dir`.
fn inside(path: &str, dir: &str) -> bool {
    path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/'
}

/// Folders first, then by name ignoring case, then by the exact name, so
/// the order is the same on every system.
fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by_cached_key(|e| (!e.kind.is_dir(), e.name.to_lowercase(), e.name.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind) -> DirEntry {
        DirEntry {
            name: name.to_owned(),
            kind,
        }
    }

    fn listing(entries: &[(&str, EntryKind)]) -> Result<DirListing, String> {
        Ok(DirListing {
            entries: entries.iter().map(|(n, k)| entry(n, *k)).collect(),
            truncated: false,
        })
    }

    use EntryKind::{Dir as D, File as F};

    /// What the rows show: two spaces per level, `/` after folders, `+`
    /// before expanded ones.
    fn shown(tree: &FileTree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|row| match row {
                Row::Entry {
                    name,
                    kind,
                    depth,
                    expanded,
                    ..
                } => format!(
                    "{}{}{name}{}",
                    "  ".repeat(*depth),
                    if *expanded { "+" } else { "" },
                    if kind.is_dir() { "/" } else { "" }
                ),
                Row::Loading { depth } => format!("{}…", "  ".repeat(*depth)),
                Row::More { depth } => format!("{}… more", "  ".repeat(*depth)),
                Row::Failed { depth, reason } => format!("{}! {reason}", "  ".repeat(*depth)),
                Row::Hit { path, .. } => format!("= {path}"),
            })
            .collect()
    }

    fn sample() -> FileTree {
        let (mut tree, root) = FileTree::new("C:/Code/demo");
        assert_eq!(root, "");
        tree.listed(
            "",
            listing(&[
                ("readme.md", F),
                ("src", D),
                ("Build.rs", F),
                ("assets", D),
                ("Zeta", D),
            ]),
        );
        tree
    }

    #[test]
    fn rf_ctx_01_folders_come_first_sorted_ignoring_case() {
        let (mut tree, _) = FileTree::new("r");
        assert_eq!(shown(&tree), ["…"], "the root is loading");
        tree.listed(
            "",
            listing(&[
                ("b.txt", F),
                ("B.txt", F),
                ("a.txt", F),
                ("Zeta", D),
                ("alpha", D),
            ]),
        );
        assert_eq!(shown(&tree), ["alpha/", "Zeta/", "a.txt", "B.txt", "b.txt"]);
    }

    #[test]
    fn rf_ctx_01_expanding_lists_one_level_and_collapsing_hides_it() {
        let mut tree = sample();
        assert_eq!(tree.expand("src"), ["src"]);
        assert_eq!(
            shown(&tree),
            ["assets/", "+src/", "  …", "Zeta/", "Build.rs", "readme.md"]
        );
        tree.listed("src", listing(&[("main.rs", F), ("ui", D)]));
        assert_eq!(tree.expand("src/ui"), ["src/ui"]);
        tree.listed("src/ui", Err("access denied".into()));
        assert_eq!(
            shown(&tree),
            [
                "assets/",
                "+src/",
                "  +ui/",
                "    ! access denied",
                "  main.rs",
                "Zeta/",
                "Build.rs",
                "readme.md"
            ]
        );
        tree.collapse("src");
        assert_eq!(
            shown(&tree),
            ["assets/", "src/", "Zeta/", "Build.rs", "readme.md"]
        );
        // Expanding again shows the last listing at once and asks again.
        assert_eq!(tree.expand("src"), ["src"]);
        assert!(shown(&tree).contains(&"  main.rs".to_owned()));
    }

    #[test]
    fn rf_ctx_01_a_cut_listing_ends_with_a_more_row() {
        let (mut tree, _) = FileTree::new("r");
        tree.listed(
            "",
            Ok(DirListing {
                entries: vec![entry("a", F)],
                truncated: true,
            }),
        );
        assert_eq!(shown(&tree), ["a", "… more"]);
        assert!(!tree.root_is_empty());
        tree.listed("", listing(&[]));
        assert!(tree.root_is_empty());
    }

    #[test]
    fn answers_for_folders_never_asked_for_are_ignored() {
        let mut tree = sample();
        tree.listed("src", listing(&[("x", F)]));
        assert!(!shown(&tree).iter().any(|r| r.contains('x')));
    }

    #[test]
    fn rf_ctx_01_refresh_asks_again_for_the_root_and_expanded_folders_only() {
        let mut tree = sample();
        tree.expand("src");
        tree.listed("src", listing(&[("main.rs", F)]));
        tree.expand("assets");
        tree.collapse("assets");
        assert_eq!(tree.refresh(), ["", "src"]);
        // Until the answers arrive, the last listings stay on screen.
        assert!(shown(&tree).contains(&"  main.rs".to_owned()));
        tree.listed("", listing(&[("src", D), ("new.txt", F)]));
        assert_eq!(shown(&tree), ["+src/", "  main.rs", "new.txt"]);
    }

    #[test]
    fn rf_ctx_01_arrows_move_expand_and_collapse() {
        let mut tree = sample();
        tree.move_cursor(1);
        assert_eq!(tree.cursor(), Some("assets"), "no cursor starts at the top");
        tree.move_cursor(1);
        assert_eq!(tree.cursor(), Some("src"));
        assert_eq!(tree.right(), ["src"]);
        tree.listed("src", listing(&[("main.rs", F)]));
        assert!(tree.right().is_empty());
        assert_eq!(tree.cursor(), Some("src/main.rs"), "right again enters it");
        tree.left();
        assert_eq!(
            tree.cursor(),
            Some("src"),
            "left on a file goes to its folder"
        );
        tree.left();
        assert_eq!(shown(&tree)[1], "src/", "left on an open folder closes it");
        tree.move_cursor(-10);
        assert_eq!(tree.cursor(), Some("assets"));
        tree.move_cursor(10);
        assert_eq!(tree.cursor(), Some("readme.md"));
    }

    #[test]
    fn rf_ctx_03_enter_opens_a_file_by_its_full_path_and_toggles_a_folder() {
        let mut tree = sample();
        tree.set_cursor("src");
        assert_eq!(tree.activate(), Activation::List(vec!["src".into()]));
        tree.listed("src", listing(&[("main.rs", F)]));
        tree.move_cursor(1);
        assert_eq!(tree.activate(), Activation::Open("src/main.rs".into()));
        let expected: PathBuf = ["C:/Code/demo", "src", "main.rs"].iter().collect();
        assert_eq!(tree.path_of("src/main.rs"), Some(expected));
        assert_eq!(tree.path_of("src/../../x"), None);
        assert_eq!(join_relative("r", "a/./b"), None);
        assert_eq!(
            join_relative("r", "a//b"),
            Some(["r", "a", "b"].iter().collect())
        );
        if cfg!(windows) {
            assert_eq!(join_relative("r", "C:/x"), None);
        }
        tree.set_cursor("src");
        assert_eq!(tree.activate(), Activation::Nothing);
        assert!(!shown(&tree).contains(&"  main.rs".to_owned()), "collapsed");
    }

    #[test]
    fn rf_ctx_03_a_click_selects_a_file_and_a_double_click_opens_it() {
        let mut tree = sample();
        assert_eq!(tree.click("readme.md", false), Activation::Nothing);
        assert_eq!(tree.cursor(), Some("readme.md"));
        assert_eq!(
            tree.click("readme.md", true),
            Activation::Open("readme.md".into())
        );
        assert_eq!(
            tree.click("assets", false),
            Activation::List(vec!["assets".into()])
        );
        // The second click of a double click leaves the folder open.
        assert_eq!(tree.click("assets", true), Activation::Nothing);
        assert!(shown(&tree).contains(&"+assets/".to_owned()));
        assert_eq!(tree.click("absent", true), Activation::Nothing);
    }

    #[test]
    fn a_folder_is_not_asked_for_twice_at_once() {
        let mut tree = sample();
        assert_eq!(tree.expand("src"), ["src"]);
        tree.collapse("src");
        assert!(tree.expand("src").is_empty(), "its listing is on the way");
        assert_eq!(tree.refresh(), [""]);
        // The refresh came while src was on the way: it is asked for again.
        assert_eq!(tree.listed("src", listing(&[("main.rs", F)])), ["src"]);
        assert!(tree.expand("src").is_empty());
        assert!(tree.listed("src", listing(&[("main.rs", F)])).is_empty());
        assert_eq!(tree.expand("src"), ["src"]);
    }

    #[test]
    fn collapsing_or_failing_forgets_the_open_folders_inside() {
        let mut tree = sample();
        tree.expand("src");
        tree.listed("src", listing(&[("ui", D)]));
        tree.expand("src/ui");
        tree.listed("src/ui", listing(&[("deep", D)]));
        tree.collapse("src");
        tree.expand("src");
        tree.listed("src", listing(&[("ui", D)]));
        assert_eq!(tree.refresh(), ["", "src"], "src/ui closed with src");
        tree.listed("", listing(&[("src", D)]));
        tree.listed("src", listing(&[("ui", D)]));
        tree.expand("src/ui");
        tree.listed("src/ui", listing(&[]));
        tree.listed("src", Err("gone".into()));
        assert_eq!(tree.refresh(), ["", "src"], "src/ui went with the failure");
    }

    #[test]
    fn rf_ctx_01_an_interrupted_search_runs_again_when_shown() {
        let mut tree = sample();
        assert!(tree.resume_search().is_none(), "no query");
        let first = tree.set_query("main").unwrap();
        tree.searched(
            first.id,
            SearchResult {
                hits: vec![],
                end: SearchEnd::Cancelled,
            },
        );
        assert!(!tree.searching());
        assert_eq!(
            tree.search_note().as_deref(),
            Some("The search was stopped.")
        );
        let again = tree.resume_search().unwrap();
        assert_eq!(again.query, "main");
        assert!(tree.awaits(again.id) && !tree.awaits(first.id));
        tree.searched(
            again.id,
            SearchResult {
                hits: vec![],
                end: SearchEnd::Unavailable,
            },
        );
        assert_eq!(
            tree.search_note().as_deref(),
            Some("The file search could not run.")
        );
        assert!(tree.resume_search().is_none(), "a finished search stays");
    }

    #[test]
    fn rf_ctx_01_the_filter_shows_the_best_matches_and_ignores_stale_answers() {
        let mut tree = sample();
        let first = tree.set_query("mai").unwrap();
        assert_eq!(tree.search_note().as_deref(), Some("Searching…"));
        let second = tree.set_query(" main ").unwrap();
        assert_eq!(second.query, "main");
        assert!(tree.set_query("main").is_none(), "the same query again");
        tree.searched(
            first.id,
            SearchResult {
                hits: vec![SearchHit {
                    path: "old".into(),
                    kind: F,
                }],
                end: SearchEnd::Complete,
            },
        );
        assert!(tree.rows().is_empty(), "the older answer is dropped");
        let hit = |path: &str, kind| SearchHit {
            path: path.into(),
            kind,
        };
        tree.searched(
            second.id,
            SearchResult {
                hits: vec![
                    hit("docs/domain.md", F),
                    hit("src/main.rs", F),
                    hit("deep/a/main.rs", F),
                ],
                end: SearchEnd::TooManyHits,
            },
        );
        assert_eq!(
            shown(&tree),
            ["= src/main.rs", "= deep/a/main.rs", "= docs/domain.md"],
            "a name that starts with the query first, shorter paths first"
        );
        assert_eq!(tree.cursor(), Some("src/main.rs"));
        assert!(
            tree.search_note()
                .unwrap()
                .starts_with("The first 3 matches")
        );
        tree.move_cursor(1);
        assert_eq!(tree.activate(), Activation::Open("deep/a/main.rs".into()));
        // Clearing the filter goes back to the tree and its cursor.
        tree.set_cursor("deep/a/main.rs");
        assert!(tree.set_query("").is_none());
        assert_eq!(tree.cursor(), None);
        assert_eq!(shown(&tree)[0], "assets/");
    }

    #[test]
    fn rf_ctx_01_a_folder_match_reveals_it_in_the_tree() {
        let mut tree = sample();
        let search = tree.set_query("ui").unwrap();
        tree.searched(
            search.id,
            SearchResult {
                hits: vec![SearchHit {
                    path: "src/ui".into(),
                    kind: D,
                }],
                end: SearchEnd::Complete,
            },
        );
        assert_eq!(
            tree.activate(),
            Activation::List(vec!["src".into(), "src/ui".into()])
        );
        assert!(!tree.filtering());
        assert_eq!(tree.query(), "");
        assert_eq!(tree.cursor(), Some("src/ui"));
        tree.listed("src", listing(&[("ui", D)]));
        tree.listed("src/ui", listing(&[]));
        assert_eq!(tree.cursor_index(&tree.rows()), Some(2));
    }

    #[test]
    fn rf_ctx_01_an_empty_search_says_so() {
        let mut tree = sample();
        let search = tree.set_query("zz").unwrap();
        tree.searched(
            search.id,
            SearchResult {
                hits: vec![],
                end: SearchEnd::Complete,
            },
        );
        assert_eq!(
            tree.search_note().as_deref(),
            Some("No file or folder name contains \"zz\".")
        );
        assert_eq!(tree.activate(), Activation::Nothing);
    }

    #[test]
    fn collapsing_moves_a_cursor_inside_onto_the_folder_only() {
        let mut tree = sample();
        tree.expand("src");
        tree.listed("src", listing(&[("main.rs", F)]));
        tree.set_cursor("src/main.rs");
        tree.collapse("srcx");
        assert_eq!(tree.cursor(), Some("src/main.rs"));
        tree.collapse("src");
        assert_eq!(tree.cursor(), Some("src"));
    }
}
