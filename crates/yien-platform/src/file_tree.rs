//! A tab's folder on disk for the file tree (RF-CTX-01): one folder level
//! per listing and a bounded search by name, both honouring `.gitignore`
//! inside a repository and hiding `.git`. Nothing here writes, starts a
//! program or follows a link while searching. The calls block; the
//! [`FileTreeService`] runs them on threads of its own.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ignore::{DirEntry, WalkBuilder};
use yien_proto::files::{
    DirEntry as Entry, DirListing, EntryKind, MAX_DIR_ENTRIES, SearchEnd, SearchHit, SearchResult,
};

/// How far a search may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchLimits {
    /// Folder levels below the root.
    pub max_depth: usize,
    /// Entries looked at.
    pub max_entries: usize,
    /// Matches kept.
    pub max_hits: usize,
    /// Time spent.
    pub max_time: Duration,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            max_depth: 16,
            max_entries: 200_000,
            max_hits: 500,
            max_time: Duration::from_secs(3),
        }
    }
}

/// The walker both calls share: hidden files shown, `.git` (a folder, or a
/// file in a linked worktree) left out, git's ignore files applied only
/// inside a repository, `.ignore` files not read, links not followed.
fn walker(dir: &Path, depth: usize) -> WalkBuilder {
    let mut builder = WalkBuilder::new(dir);
    builder
        .hidden(false)
        .ignore(false)
        .parents(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(true)
        .follow_links(false)
        .max_depth(Some(depth))
        .filter_entry(|entry| !is_git_dir_name(entry.file_name()));
    builder
}

/// `.git`, and on Windows, where names ignore case, `.GIT` too.
fn is_git_dir_name(name: &std::ffi::OsStr) -> bool {
    if cfg!(windows) {
        name.eq_ignore_ascii_case(".git")
    } else {
        name == ".git"
    }
}

/// The folder `rel` names under `root`: parts joined with `/`, as listings
/// and searches give them. `None` for a part that would leave `root`.
///
/// ```
/// use std::path::Path;
/// use yien_platform::file_tree::resolve;
///
/// assert_eq!(resolve(Path::new("r"), "a/b"), Some(Path::new("r").join("a").join("b")));
/// assert_eq!(resolve(Path::new("r"), ""), Some(Path::new("r").to_path_buf()));
/// assert_eq!(resolve(Path::new("r"), "a/../b"), None);
/// ```
pub fn resolve(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        // On Windows `\` separates and `C:` would replace the whole path.
        let foreign = cfg!(windows) && part.contains(['\\', ':']);
        if part == "." || part == ".." || foreign || Path::new(part).is_absolute() {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

/// Lists one level of the folder `rel` under `root`, at most
/// [`MAX_DIR_ENTRIES`] entries, in no particular order. Entries that cannot
/// be read are left out; a folder that cannot be read is an error.
///
/// ```no_run
/// let listing = yien_platform::file_tree::list_dir(std::path::Path::new("C:/Code/yien"), "crates")?;
/// println!("{} entries", listing.entries.len());
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn list_dir(root: &Path, rel: &str) -> io::Result<DirListing> {
    let dir = resolve(root, rel).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the folder is outside the tab's folder",
        )
    })?;
    if !std::fs::metadata(&dir)?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a folder"));
    }
    // An unreadable folder would come back as one error and no entries.
    std::fs::read_dir(&dir)?;
    let mut listing = DirListing::default();
    for entry in walker(&dir, 1).build() {
        let Ok(entry) = entry else { continue };
        if entry.depth() == 0 {
            continue;
        }
        if listing.entries.len() == MAX_DIR_ENTRIES {
            listing.truncated = true;
            break;
        }
        listing.entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind: kind_of(&entry),
        });
    }
    Ok(listing)
}

fn kind_of(entry: &DirEntry) -> EntryKind {
    if entry.path_is_symlink() {
        // One look at the target; a dangling link is a file.
        let dir = std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir());
        return EntryKind::Link { dir };
    }
    match entry.file_type() {
        Some(t) if t.is_dir() => EntryKind::Dir,
        _ => EntryKind::File,
    }
}

/// Searches `root` for entries whose name holds `query`, ignoring case,
/// within `limits`. `cancelled` is asked between entries; once it says yes
/// the search stops with what it found. Paths are relative to `root`, parts
/// joined with `/`. An empty query finds nothing.
///
/// ```no_run
/// use yien_platform::file_tree::{SearchLimits, search};
///
/// let found = search(std::path::Path::new("C:/Code/yien"), "main", SearchLimits::default(), &|| false);
/// for hit in found.hits {
///     println!("{}", hit.path);
/// }
/// ```
pub fn search(
    root: &Path,
    query: &str,
    limits: SearchLimits,
    cancelled: &dyn Fn() -> bool,
) -> SearchResult {
    let needle = query.trim().to_lowercase();
    let mut hits = Vec::new();
    if needle.is_empty() {
        return SearchResult {
            hits,
            end: SearchEnd::Complete,
        };
    }
    let started = Instant::now();
    let mut builder = walker(root, limits.max_depth);
    builder.sort_by_file_name(|a, b| a.cmp(b));
    for (seen, entry) in builder.build().enumerate() {
        if cancelled() {
            return SearchResult {
                hits,
                end: SearchEnd::Cancelled,
            };
        }
        if seen.is_multiple_of(256) && started.elapsed() >= limits.max_time {
            return SearchResult {
                hits,
                end: SearchEnd::TimedOut,
            };
        }
        if seen == limits.max_entries {
            return SearchResult {
                hits,
                end: SearchEnd::TooManyEntries,
            };
        }
        let Ok(entry) = entry else { continue };
        if entry.depth() == 0 {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !name.to_lowercase().contains(&needle) {
            continue;
        }
        let Some(path) = relative(root, entry.path()) else {
            continue;
        };
        hits.push(SearchHit {
            path,
            kind: kind_of(&entry),
        });
        if hits.len() == limits.max_hits {
            return SearchResult {
                hits,
                end: SearchEnd::TooManyHits,
            };
        }
    }
    SearchResult {
        hits,
        end: SearchEnd::Complete,
    }
}

/// `path` below `root` as its parts joined with `/`.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// What a listing hands back.
pub type ListDone = Box<dyn FnOnce(io::Result<DirListing>) + Send>;

/// What a search hands back.
pub type SearchDone = Box<dyn FnOnce(SearchResult) + Send>;

struct ListJob {
    root: PathBuf,
    rel: String,
    done: ListDone,
}

/// Runs listings one at a time on a thread of its own, and each search on
/// a thread of its own, a newer search cancelling the one before. Calls
/// return at once; the callbacks run on those threads. Dropping the service
/// ends the listing thread after the job in hand and cancels the search.
///
/// ```no_run
/// use yien_platform::file_tree::FileTreeService;
///
/// let files = FileTreeService::start()?;
/// files.list("C:/Code/yien".into(), String::new(), Box::new(|listing| {
///     println!("{:?}", listing.map(|l| l.entries.len()));
/// }));
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct FileTreeService {
    jobs: mpsc::Sender<ListJob>,
    /// The newest search's number; an older one sees it changed and stops.
    latest: Arc<AtomicU64>,
    limits: SearchLimits,
}

impl FileTreeService {
    /// Starts the listing thread.
    pub fn start() -> io::Result<Self> {
        Self::with_limits(SearchLimits::default())
    }

    /// Starts the listing thread, searches bounded by `limits`.
    pub fn with_limits(limits: SearchLimits) -> io::Result<Self> {
        let (jobs, queue) = mpsc::channel::<ListJob>();
        std::thread::Builder::new()
            .name("yien-file-tree".into())
            .spawn(move || {
                for job in queue {
                    (job.done)(list_dir(&job.root, &job.rel));
                }
            })?;
        Ok(Self {
            jobs,
            latest: Arc::new(AtomicU64::new(0)),
            limits,
        })
    }

    /// Lists the folder `rel` under `root`.
    pub fn list(&self, root: PathBuf, rel: String, done: ListDone) {
        if let Err(mpsc::SendError(job)) = self.jobs.send(ListJob { root, rel, done }) {
            (job.done)(Err(io::Error::other("the file tree thread stopped")));
        }
    }

    /// Searches `root` by name, cancelling the search before it.
    pub fn search(&self, root: PathBuf, query: String, done: SearchDone) {
        let mine = self.latest.fetch_add(1, Ordering::AcqRel) + 1;
        let latest = Arc::clone(&self.latest);
        let limits = self.limits;
        let spawned = std::thread::Builder::new()
            .name("yien-file-search".into())
            .spawn(move || {
                let cancelled = || latest.load(Ordering::Acquire) != mine;
                done(search(&root, &query, limits, &cancelled));
            });
        if let Err(e) = spawned {
            // The callback went with the closure; the caller sees it dropped.
            tracing::warn!("could not start a file search: {e}");
        }
    }

    /// Stops the running search, if any; it ends as cancelled.
    pub fn cancel_search(&self) {
        self.latest.fetch_add(1, Ordering::AcqRel);
    }
}

impl Drop for FileTreeService {
    fn drop(&mut self) {
        self.cancel_search();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("yien-file-tree-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, rel: &str) {
            let path = resolve(&self.0, rel).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        }

        fn dir(&self, rel: &str) {
            fs::create_dir_all(resolve(&self.0, rel).unwrap()).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(listing: &DirListing) -> Vec<&str> {
        let mut names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        names.sort_unstable();
        names
    }

    fn paths(result: &SearchResult) -> Vec<&str> {
        result.hits.iter().map(|h| h.path.as_str()).collect()
    }

    /// A repository for the walker: `ignore` only needs `.git` to exist.
    fn repo(name: &str) -> Scratch {
        let s = Scratch::new(name);
        s.dir(".git/objects");
        s.file(".git/HEAD");
        s.file(".gitignore");
        fs::write(s.0.join(".gitignore"), "generated/\n*.tmpout\n").unwrap();
        s.file("alpha.txt");
        s.file("scratch.tmpout");
        s.file("generated/out.txt");
        s.file("src/lib.rs");
        s.file("src/generated/kept.rs");
        s.file(".github/flow.yml");
        s
    }

    #[test]
    fn rf_ctx_01_a_repository_lists_one_level_without_ignored_entries_or_git() {
        let s = repo("list");
        let root = list_dir(&s.0, "").unwrap();
        assert_eq!(
            names(&root),
            [".github", ".gitignore", "alpha.txt", "src"],
            "generated/ and *.tmpout are ignored, .git is hidden, dotfiles show"
        );
        assert!(!root.truncated);
        let kind = |n: &str| root.entries.iter().find(|e| e.name == n).unwrap().kind;
        assert_eq!(kind("src"), EntryKind::Dir);
        assert_eq!(kind("alpha.txt"), EntryKind::File);
        // The root's .gitignore applies when a subfolder is listed alone:
        // `generated/` matches at any depth.
        assert!(names(&list_dir(&s.0, "src").unwrap()) == ["lib.rs"]);
    }

    #[test]
    fn rf_ctx_01_a_plain_folder_shows_everything_but_git() {
        let s = Scratch::new("plain");
        fs::write(s.0.join(".gitignore"), "generated/\n").unwrap();
        s.file("generated/out.txt");
        s.file("note.md");
        assert_eq!(
            names(&list_dir(&s.0, "").unwrap()),
            [".gitignore", "generated", "note.md"]
        );
        // A linked worktree's .git is a file: hidden, and the folder is a
        // repository from then on.
        s.file(".git");
        assert_eq!(
            names(&list_dir(&s.0, "").unwrap()),
            [".gitignore", "note.md"]
        );
    }

    #[test]
    fn rf_ctx_01_a_big_folder_is_cut_at_the_limit() {
        let s = Scratch::new("big");
        for i in 0..MAX_DIR_ENTRIES + 3 {
            fs::write(s.0.join(format!("f{i}")), "").unwrap();
        }
        let listing = list_dir(&s.0, "").unwrap();
        assert_eq!(listing.entries.len(), MAX_DIR_ENTRIES);
        assert!(listing.truncated);
    }

    #[test]
    fn listing_refuses_paths_that_leave_the_root_and_reports_missing_folders() {
        let s = Scratch::new("refuse");
        s.file("a.txt");
        assert_eq!(
            list_dir(&s.0, "../x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            list_dir(&s.0, "absent").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(list_dir(&s.0, "a.txt").is_err(), "a file is not a folder");
    }

    #[test]
    fn rf_ctx_01_search_matches_names_ignoring_case_and_skips_ignored_entries() {
        let s = repo("search");
        s.file("src/deep/a/b/Kept_Notes.md");
        let found = search(&s.0, "KEPT", SearchLimits::default(), &|| false);
        assert_eq!(found.end, SearchEnd::Complete);
        // `generated/` is ignored at any depth, so src/generated/kept.rs is not found.
        assert_eq!(paths(&found), ["src/deep/a/b/Kept_Notes.md"]);
        let gen_found = search(&s.0, "out", SearchLimits::default(), &|| false);
        assert!(paths(&gen_found).is_empty(), "{:?}", paths(&gen_found));
        let dirs = search(&s.0, "sr", SearchLimits::default(), &|| false);
        assert_eq!(paths(&dirs), ["src"]);
        assert_eq!(dirs.hits[0].kind, EntryKind::Dir);
        assert!(
            search(&s.0, "  ", SearchLimits::default(), &|| false)
                .hits
                .is_empty()
        );
        assert!(
            search(&s.0, "HEAD", SearchLimits::default(), &|| false)
                .hits
                .is_empty(),
            ".git is never searched"
        );
    }

    #[test]
    fn rf_ctx_01_search_stops_at_its_limits_and_when_cancelled() {
        let s = Scratch::new("limits");
        for i in 0..20 {
            s.file(&format!("d{i}/match{i}.txt"));
        }
        s.file("a/b/c/d/match-deep.txt");
        let base = SearchLimits::default();
        let hits = search(
            &s.0,
            "match",
            SearchLimits {
                max_hits: 5,
                ..base
            },
            &|| false,
        );
        assert_eq!(hits.hits.len(), 5);
        assert_eq!(hits.end, SearchEnd::TooManyHits);
        let entries = search(
            &s.0,
            "match",
            SearchLimits {
                max_entries: 10,
                ..base
            },
            &|| false,
        );
        assert_eq!(entries.end, SearchEnd::TooManyEntries);
        let shallow = search(
            &s.0,
            "deep",
            SearchLimits {
                max_depth: 3,
                ..base
            },
            &|| false,
        );
        assert!(shallow.hits.is_empty());
        let deep = search(
            &s.0,
            "deep",
            SearchLimits {
                max_depth: 5,
                ..base
            },
            &|| false,
        );
        assert_eq!(paths(&deep), ["a/b/c/d/match-deep.txt"]);
        let timed = search(
            &s.0,
            "match",
            SearchLimits {
                max_time: Duration::ZERO,
                ..base
            },
            &|| false,
        );
        assert_eq!(timed.end, SearchEnd::TimedOut);
        let cancelled = search(&s.0, "match", base, &|| true);
        assert_eq!(cancelled.end, SearchEnd::Cancelled);
        assert!(cancelled.hits.is_empty());
    }

    #[cfg(unix)]
    fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[test]
    fn rf_ctx_01_links_are_listed_and_a_link_loop_does_not_trap_the_search() {
        let s = Scratch::new("loop");
        s.file("inner/loop-target.txt");
        if let Err(e) = link_dir(&s.0, &s.0.join("inner").join("back")) {
            // Windows needs a privilege (or developer mode) to make links.
            eprintln!("skipped: cannot create a directory link here: {e}");
            return;
        }
        let inner = list_dir(&s.0, "inner").unwrap();
        let back = inner.entries.iter().find(|e| e.name == "back").unwrap();
        assert_eq!(back.kind, EntryKind::Link { dir: true });
        let found = search(&s.0, "loop", SearchLimits::default(), &|| false);
        assert_eq!(found.end, SearchEnd::Complete);
        assert_eq!(
            paths(&found),
            ["inner/loop-target.txt"],
            "the link is never followed"
        );
        let link = search(&s.0, "back", SearchLimits::default(), &|| false);
        assert_eq!(link.hits[0].kind, EntryKind::Link { dir: true });
    }

    #[test]
    fn the_service_lists_and_searches() {
        let s = repo("service");
        let service = FileTreeService::start().unwrap();
        let (tx, rx) = mpsc::channel();
        service.list(
            s.0.clone(),
            "src".into(),
            Box::new(move |l| {
                let _ = tx.send(l.map(|l| l.entries.len()));
            }),
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap(),
            1
        );
        let (tx, rx) = mpsc::channel();
        service.search(
            s.0.clone(),
            "alpha".into(),
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        );
        let found = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(found.end, SearchEnd::Complete);
        assert_eq!(paths(&found), ["alpha.txt"]);
    }

    #[test]
    fn rf_ctx_01_a_search_stops_mid_walk_once_cancelled() {
        let s = Scratch::new("midwalk");
        for i in 0..40 {
            s.file(&format!("d{i:02}/hit{i}.txt"));
        }
        let asked = std::cell::Cell::new(0);
        let cancelled = || {
            asked.set(asked.get() + 1);
            asked.get() > 10
        };
        let found = search(&s.0, "hit", SearchLimits::default(), &cancelled);
        assert_eq!(found.end, SearchEnd::Cancelled);
        assert!(
            !found.hits.is_empty() && found.hits.len() < 40,
            "{}",
            found.hits.len()
        );
    }

    #[test]
    fn rf_ctx_01_the_service_cancels_the_older_search() {
        let s = Scratch::new("cancel");
        for i in 0..1_000 {
            s.file(&format!("d{}/hit{i}.txt", i % 50));
        }
        let service = FileTreeService::start().unwrap();
        let (first_tx, first) = mpsc::channel();
        service.search(
            s.0.clone(),
            "hit".into(),
            Box::new(move |r| {
                let _ = first_tx.send(r.end);
            }),
        );
        // A newer search moves the number on before the older one has
        // walked 1,000 entries.
        let (second_tx, second) = mpsc::channel();
        service.search(
            s.0.clone(),
            "hit99".into(),
            Box::new(move |r| {
                let _ = second_tx.send(r);
            }),
        );
        assert_eq!(
            first.recv_timeout(Duration::from_secs(20)).unwrap(),
            SearchEnd::Cancelled
        );
        let found = second.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(found.end, SearchEnd::Complete);
        assert_eq!(found.hits.len(), 11, "hit99 and hit990 to hit999");
        let (third_tx, third) = mpsc::channel();
        service.search(
            s.0.clone(),
            "hit".into(),
            Box::new(move |r| {
                let _ = third_tx.send(r.end);
            }),
        );
        service.cancel_search();
        assert_eq!(
            third.recv_timeout(Duration::from_secs(20)).unwrap(),
            SearchEnd::Cancelled
        );
    }

    /// A junction needs no privilege, unlike a directory symlink.
    #[cfg(windows)]
    #[test]
    fn rf_ctx_01_a_junction_loop_is_listed_and_never_followed() {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let s = Scratch::new("junction");
        s.file("inner/loop-target.txt");
        let made = std::process::Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(s.0.join("inner").join("back"))
            .arg(&s.0)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(made.success());
        let inner = list_dir(&s.0, "inner").unwrap();
        let back = inner.entries.iter().find(|e| e.name == "back").unwrap();
        assert_eq!(back.kind, EntryKind::Link { dir: true });
        let found = search(&s.0, "loop", SearchLimits::default(), &|| false);
        assert_eq!(found.end, SearchEnd::Complete);
        assert_eq!(paths(&found), ["inner/loop-target.txt"]);
        // Removing the junction, not its target, before the scratch goes.
        fs::remove_dir(s.0.join("inner").join("back")).unwrap();
    }
}
