//! The Files view's context menu actions on disk (RF-CTX-01, RNF-14): new
//! file or folder, rename, duplicate, paste, trash, delete, `.gitignore` and
//! reveal. Every path is resolved (`..`, junctions, symlinks) and must stay
//! inside the tab's folder; nothing is written inside `.git`, and nothing is
//! ever overwritten. The calls block; the core runs them off its loop.
//!
//! ```no_run
//! use yien_platform::file_ops;
//! use yien_proto::file_ops::FileAction;
//!
//! let done = file_ops::run(
//!     std::path::Path::new("C:/Code/yien"),
//!     &FileAction::NewFile { dir: "notes".into(), name: "todo.md".into() },
//! )?;
//! assert_eq!(done.select.as_deref(), Some("notes/todo.md"));
//! # Ok::<(), yien_platform::file_ops::FileOpError>(())
//! ```

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use yien_proto::file_ops::{
    FileAction, check_name, free_copy_name, gitignore_addition, gitignore_entry, has_control,
    is_git_path, paste_name, rel_join, rel_name, rel_parent, win32_rewrites,
};
use yien_proto::path_within;

use crate::file_tree::resolve;

#[cfg(windows)]
mod windows;

/// The largest `.gitignore` YIEN reads to add a line.
const MAX_GITIGNORE: u64 = 4 * 1024 * 1024;

/// How many folder levels below the copied folder a copy goes; deeper is
/// refused before anything is made.
pub const MAX_COPY_DEPTH: usize = 128;

/// Why a file action was refused or failed. Each says it for the user; a
/// refused action changed nothing.
#[derive(Debug, thiserror::Error)]
pub enum FileOpError {
    /// The path, once resolved, leaves the tab's folder.
    #[error("{0} is outside the tab's folder, so YIEN left it alone.")]
    Outside(String),
    /// The action would write inside `.git`.
    #[error("YIEN does not change anything inside .git.")]
    GitFolder,
    /// The name typed cannot be a file name.
    #[error("{0}")]
    BadName(String),
    /// Something already has that name; nothing is overwritten.
    #[error("{0} already exists.")]
    Exists(String),
    /// The entry is not there any more.
    #[error("{0} is not there any more.")]
    Missing(String),
    /// The action cannot be done to the tab's folder itself.
    #[error("The tab's folder itself cannot be {0}.")]
    Root(&'static str),
    /// A folder would go inside itself.
    #[error("{0} cannot go inside itself.")]
    IntoItself(String),
    /// Copying a link, or a folder that holds one, is not supported.
    #[error("{0} is or holds a link; YIEN copies only plain files and folders.")]
    Link(String),
    /// The target of a new entry or a paste is not a folder.
    #[error("{0} is not a folder.")]
    NotAFolder(String),
    /// Duplicate copies files only.
    #[error("{0} is a folder; Duplicate copies files only.")]
    NotAFile(String),
    /// Every copy name was taken.
    #[error("There is no free name for a copy of {0}.")]
    NoFreeName(String),
    /// A path that is not valid Unicode cannot be checked.
    #[error("{0} has a name YIEN cannot read.")]
    Unreadable(String),
    /// The `.gitignore` is too big or not text.
    #[error("The .gitignore is not a text file YIEN can add to.")]
    BadGitignore,
    /// The `.gitignore` is a link, which could lead out of the folder.
    #[error("The .gitignore is a link, so YIEN does not write through it.")]
    GitignoreLink,
    /// The Windows shell would read the name as another item.
    #[error(
        "The Windows shell reads the name of {0} as another item, so YIEN left it alone; \
         Delete Permanently works on it."
    )]
    ShellName(String),
    /// A folder to copy is nested too deep.
    #[error("{0} holds folders more than {MAX_COPY_DEPTH} levels deep; YIEN does not copy it.")]
    TooDeep(String),
    /// The OS has no such action here yet.
    #[error("{0} is not available on this system yet.")]
    Unsupported(&'static str),
    /// The OS refused.
    #[error("Could not {what}: {source}")]
    Io {
        /// What was being done, for the user.
        what: String,
        /// Why.
        #[source]
        source: io::Error,
    },
}

fn io_error(what: impl Into<String>) -> impl FnOnce(io::Error) -> FileOpError {
    let what = what.into();
    move |source| FileOpError::Io { what, source }
}

/// What a finished action changed, for the file tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Done {
    /// Folders (relative, `""` for the root) whose listing changed.
    pub dirs: Vec<String>,
    /// The entry made or moved, for the tree's cursor.
    pub select: Option<String>,
}

/// Runs `action` on the folder `root`.
///
/// # Errors
/// A refusal ([`FileOpError::Outside`], [`FileOpError::GitFolder`],
/// [`FileOpError::Exists`], ...) changes nothing; an OS failure in the
/// middle of copying a folder removes what the copy made.
pub fn run(root: &Path, action: &FileAction) -> Result<Done, FileOpError> {
    if action.paths().iter().any(|p| has_control(p)) {
        return Err(control_error());
    }
    let folder = Folder::open(root)?;
    match action {
        FileAction::NewFile { dir, name } => folder.new_entry(dir, name, false),
        FileAction::NewFolder { dir, name } => folder.new_entry(dir, name, true),
        FileAction::Rename { path, name } => folder.rename(path, name),
        FileAction::Duplicate { path } => folder.duplicate(path),
        FileAction::Paste { from, into, cut } => folder.paste(from, into, *cut),
        FileAction::Trash { path } => folder.trash(path),
        FileAction::Delete { path } => folder.delete(path),
        FileAction::Ignore { path } => folder.ignore(path),
        FileAction::Reveal { path } => folder.reveal(path),
    }
}

fn control_error() -> FileOpError {
    FileOpError::BadName("A path cannot contain control characters.".to_owned())
}

/// The folder `rel` of the tab folder `root`, resolved (links followed) and
/// inside it, for a shell to start in.
///
/// # Errors
/// [`FileOpError::Outside`] when it resolves out of `root`,
/// [`FileOpError::NotAFolder`] or [`FileOpError::Missing`] when it is not a
/// folder there.
///
/// ```no_run
/// let dir = yien_platform::file_ops::terminal_folder(std::path::Path::new("C:/Code/yien"), "crates")?;
/// assert!(dir.is_dir());
/// # Ok::<(), yien_platform::file_ops::FileOpError>(())
/// ```
pub fn terminal_folder(root: &Path, rel: &str) -> Result<PathBuf, FileOpError> {
    if has_control(rel) {
        return Err(control_error());
    }
    Folder::open(root)?.folder_at(rel, false)
}

/// How a relative path reads to the user: `the tab's folder` for the root.
fn shown(rel: &str) -> String {
    if rel.is_empty() {
        "The tab's folder".to_owned()
    } else {
        rel.to_owned()
    }
}

/// The tab's folder, resolved once per action.
struct Folder {
    root: PathBuf,
    root_text: String,
}

impl Folder {
    fn open(root: &Path) -> Result<Self, FileOpError> {
        let root = fs::canonicalize(root).map_err(io_error("open the tab's folder"))?;
        let root_text = root
            .to_str()
            .ok_or_else(|| FileOpError::Unreadable("The tab's folder".to_owned()))?
            .to_owned();
        Ok(Self { root, root_text })
    }

    /// Refuses `path` (resolved) unless it lies inside the folder, and, when
    /// `writes`, unless it is outside `.git`.
    fn check(&self, rel: &str, path: &Path, writes: bool) -> Result<(), FileOpError> {
        let text = path
            .to_str()
            .ok_or_else(|| FileOpError::Unreadable(shown(rel)))?;
        if !path_within(&self.root_text, text, cfg!(windows)) {
            return Err(FileOpError::Outside(shown(rel)));
        }
        if writes {
            let below = path
                .strip_prefix(&self.root)
                .map_err(|_| FileOpError::Outside(shown(rel)))?;
            let below: Vec<String> = below
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if is_git_path(rel, cfg!(windows)) || is_git_path(&below.join("/"), cfg!(windows)) {
                return Err(FileOpError::GitFolder);
            }
        }
        Ok(())
    }

    /// The entry `rel` itself, not followed when it is a link: its folder
    /// resolved, then its own long name (an 8.3 name like `GIT~1` becomes
    /// `.git` before the checks). It must exist.
    fn entry(&self, rel: &str, writes: bool) -> Result<PathBuf, FileOpError> {
        let lexical = resolve(&self.root, rel).ok_or_else(|| FileOpError::Outside(shown(rel)))?;
        if writes && is_git_path(rel, cfg!(windows)) {
            return Err(FileOpError::GitFolder);
        }
        let path = if rel.split('/').all(str::is_empty) {
            self.root.clone()
        } else {
            let (Some(parent), Some(name)) = (lexical.parent(), lexical.file_name()) else {
                return Err(FileOpError::Outside(shown(rel)));
            };
            let parent = fs::canonicalize(parent).map_err(|e| missing_or(e, rel))?;
            let path = parent.join(name);
            let meta = fs::symlink_metadata(&path).map_err(|e| missing_or(e, rel))?;
            if meta.file_type().is_symlink() {
                listed_by_own_name(&parent, name, rel)?;
                path
            } else {
                fs::canonicalize(&path).map_err(|e| missing_or(e, rel))?
            }
        };
        self.check(rel, &path, writes)?;
        Ok(path)
    }

    /// The folder `rel`, links followed, to put something in.
    fn folder(&self, rel: &str) -> Result<PathBuf, FileOpError> {
        self.folder_at(rel, true)
    }

    /// The folder `rel`, links followed and inside the tab folder; with
    /// `writes`, outside `.git` too.
    fn folder_at(&self, rel: &str, writes: bool) -> Result<PathBuf, FileOpError> {
        let lexical = resolve(&self.root, rel).ok_or_else(|| FileOpError::Outside(shown(rel)))?;
        let path = fs::canonicalize(&lexical).map_err(|e| missing_or(e, rel))?;
        self.check(rel, &path, writes)?;
        if !path.is_dir() {
            return Err(FileOpError::NotAFolder(shown(rel)));
        }
        Ok(path)
    }

    /// The entry `rel` as the Windows shell will be handed it: refused when
    /// a part of its path is a name Win32 reads as another item (`foo.`,
    /// `...`, `nul`), since the shell takes no `\\?\` path.
    fn shell_target(&self, rel: &str, writes: bool) -> Result<PathBuf, FileOpError> {
        let path = self.entry(rel, writes)?;
        if cfg!(windows) && shell_rewrites(&path) {
            return Err(FileOpError::ShellName(shown(rel)));
        }
        Ok(path)
    }

    /// The entry `rel` to copy from: a plain file or folder, its real place
    /// inside the folder.
    fn source(&self, rel: &str) -> Result<(PathBuf, fs::Metadata), FileOpError> {
        let path = self.entry(rel, false)?;
        let meta = fs::symlink_metadata(&path).map_err(|e| missing_or(e, rel))?;
        if meta.file_type().is_symlink() {
            return Err(FileOpError::Link(shown(rel)));
        }
        Ok((path, meta))
    }

    fn new_entry(&self, dir: &str, name: &str, folder: bool) -> Result<Done, FileOpError> {
        check_name(name, cfg!(windows)).map_err(FileOpError::BadName)?;
        let into = self.folder(dir)?;
        let rel = rel_join(dir, name);
        let target = into.join(name);
        self.check(&rel, &target, true)?;
        let made = if folder {
            fs::create_dir(&target)
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map(drop)
        };
        made.map_err(|e| exists_or(e, &rel, "create it"))?;
        Ok(Done {
            dirs: vec![dir.to_owned()],
            select: Some(rel),
        })
    }

    fn rename(&self, rel: &str, name: &str) -> Result<Done, FileOpError> {
        if rel.split('/').all(str::is_empty) {
            return Err(FileOpError::Root("renamed"));
        }
        check_name(name, cfg!(windows)).map_err(FileOpError::BadName)?;
        let from = self.entry(rel, true)?;
        let dir = rel_parent(rel);
        let renamed = rel_join(dir, name);
        let done = Done {
            dirs: vec![dir.to_owned()],
            select: Some(renamed.clone()),
        };
        if rel_name(rel) == name {
            return Ok(done);
        }
        let Some(parent) = from.parent() else {
            return Err(FileOpError::Root("renamed"));
        };
        let to = parent.join(name);
        self.check(&renamed, &to, true)?;
        move_new(&from, &to).map_err(|e| exists_or(e, &renamed, "rename it"))?;
        Ok(done)
    }

    fn duplicate(&self, rel: &str) -> Result<Done, FileOpError> {
        let (from, meta) = self.source(rel)?;
        if meta.is_dir() {
            return Err(FileOpError::NotAFile(shown(rel)));
        }
        let dir = rel_parent(rel);
        let into = self.folder(dir)?;
        let name = free_copy_name(rel_name(rel), false, |n| taken(&into, n))
            .ok_or_else(|| FileOpError::NoFreeName(shown(rel)))?;
        let copy = rel_join(dir, &name);
        let target = into.join(&name);
        self.check(&copy, &target, true)?;
        copy_file_new(&from, &target).map_err(|e| exists_or(e, &copy, "copy it"))?;
        Ok(Done {
            dirs: vec![dir.to_owned()],
            select: Some(copy),
        })
    }

    fn paste(&self, from_rel: &str, into_rel: &str, cut: bool) -> Result<Done, FileOpError> {
        if from_rel.split('/').all(str::is_empty) {
            return Err(FileOpError::Root(if cut { "moved" } else { "copied" }));
        }
        let into = self.folder(into_rel)?;
        let (from, meta) = if cut {
            let from = self.entry(from_rel, true)?;
            let meta = fs::symlink_metadata(&from).map_err(|e| missing_or(e, from_rel))?;
            (from, meta)
        } else {
            self.source(from_rel)?
        };
        let is_dir = meta.is_dir();
        if is_dir {
            let from_text = from
                .to_str()
                .ok_or_else(|| FileOpError::Unreadable(shown(from_rel)))?;
            let into_text = into
                .to_str()
                .ok_or_else(|| FileOpError::Unreadable(shown(into_rel)))?;
            if path_within(from_text, into_text, cfg!(windows)) {
                return Err(FileOpError::IntoItself(shown(from_rel)));
            }
        }
        let same_folder = from.parent().is_some_and(|p| p == into);
        if cut && same_folder {
            return Ok(Done {
                dirs: Vec::new(),
                select: Some(from_rel.to_owned()),
            });
        }
        let name = paste_name(rel_name(from_rel), is_dir, |n| taken(&into, n))
            .ok_or_else(|| FileOpError::NoFreeName(shown(from_rel)))?;
        let pasted = rel_join(into_rel, &name);
        let target = into.join(&name);
        self.check(&pasted, &target, true)?;
        if cut {
            move_new(&from, &target).map_err(|e| exists_or(e, &pasted, "move it"))?;
        } else if is_dir {
            copy_dir_new(&from, &target, from_rel)?;
        } else {
            copy_file_new(&from, &target).map_err(|e| exists_or(e, &pasted, "copy it"))?;
        }
        let mut dirs = vec![into_rel.to_owned()];
        if cut {
            dirs.push(rel_parent(from_rel).to_owned());
        }
        Ok(Done {
            dirs,
            select: Some(pasted),
        })
    }

    fn trash(&self, rel: &str) -> Result<Done, FileOpError> {
        if rel.split('/').all(str::is_empty) {
            return Err(FileOpError::Root("moved to the trash"));
        }
        let path = self.shell_target(rel, true)?;
        move_to_trash(&path)?;
        Ok(Done {
            dirs: vec![rel_parent(rel).to_owned()],
            select: None,
        })
    }

    fn delete(&self, rel: &str) -> Result<Done, FileOpError> {
        if rel.split('/').all(str::is_empty) {
            return Err(FileOpError::Root("deleted"));
        }
        let path = self.entry(rel, true)?;
        remove_entry(&path).map_err(io_error(format!("delete {rel}")))?;
        Ok(Done {
            dirs: vec![rel_parent(rel).to_owned()],
            select: None,
        })
    }

    fn ignore(&self, rel: &str) -> Result<Done, FileOpError> {
        if rel.split('/').all(str::is_empty) {
            return Err(FileOpError::Root("ignored"));
        }
        let path = self.entry(rel, false)?;
        // A link is a file to git, whatever it points at.
        let is_dir = fs::symlink_metadata(&path)
            .map_err(|e| missing_or(e, rel))?
            .is_dir();
        let line = gitignore_entry(rel, is_dir);
        let file = self.root.join(".gitignore");
        self.check(".gitignore", &file, true)?;
        // A link, even a dangling one, could create or change a file outside.
        let existing = match fs::symlink_metadata(&file) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(FileOpError::GitignoreLink),
            Ok(_) => Some(read_text(&file)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error("read the .gitignore")(e)),
        };
        let content = existing.as_deref().unwrap_or_default();
        if let Some(addition) = gitignore_addition(content, &line) {
            let mut options = OpenOptions::new();
            if existing.is_some() {
                options.append(true);
            } else {
                options.write(true).create_new(true);
            }
            options
                .open(&file)
                .and_then(|mut f| f.write_all(addition.as_bytes()))
                .map_err(io_error("add to the .gitignore"))?;
        }
        let mut dirs = vec![String::new()];
        let parent = rel_parent(rel);
        if !parent.is_empty() {
            dirs.push(parent.to_owned());
        }
        Ok(Done { dirs, select: None })
    }

    fn reveal(&self, rel: &str) -> Result<Done, FileOpError> {
        let path = self.shell_target(rel, false)?;
        show_in_file_manager(&path)?;
        Ok(Done::default())
    }
}

/// Refuses a link `name` in `parent` unless the folder lists it by that very
/// name: a short 8.3 name would reach it without being its name.
fn listed_by_own_name(parent: &Path, name: &std::ffi::OsStr, rel: &str) -> Result<(), FileOpError> {
    let listing = fs::read_dir(parent).map_err(|e| missing_or(e, rel))?;
    if listing.flatten().any(|e| e.file_name() == name) {
        Ok(())
    } else {
        Err(FileOpError::Missing(shown(rel)))
    }
}

/// Whether a part of `path` is a name Win32 path parsing rewrites.
fn shell_rewrites(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(part) => part.to_str().is_none_or(win32_rewrites),
        Component::CurDir | Component::ParentDir => true,
        Component::Prefix(_) | Component::RootDir => false,
    })
}

/// `NotFound` as [`FileOpError::Missing`], anything else as an OS failure.
fn missing_or(e: io::Error, rel: &str) -> FileOpError {
    if e.kind() == io::ErrorKind::NotFound {
        FileOpError::Missing(shown(rel))
    } else {
        io_error(format!("open {}", shown(rel)))(e)
    }
}

/// `AlreadyExists` as [`FileOpError::Exists`], anything else as an OS
/// failure while doing `what`.
fn exists_or(e: io::Error, rel: &str, what: &str) -> FileOpError {
    if e.kind() == io::ErrorKind::AlreadyExists {
        FileOpError::Exists(rel_name(rel).to_owned())
    } else {
        io_error(what)(e)
    }
}

/// Whether `name` in `dir` is in use, by anything, dangling links included.
fn taken(dir: &Path, name: &str) -> bool {
    fs::symlink_metadata(dir.join(name)).is_ok()
}

fn read_text(path: &Path) -> Result<String, FileOpError> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|f| f.take(MAX_GITIGNORE + 1).read_to_end(&mut bytes))
        .map_err(io_error("read the .gitignore"))?;
    if bytes.len() as u64 > MAX_GITIGNORE {
        return Err(FileOpError::BadGitignore);
    }
    String::from_utf8(bytes).map_err(|_| FileOpError::BadGitignore)
}

/// Copies the file `from` to `to`, which must not exist; a failure leaves
/// no partial `to` behind.
fn copy_file_new(from: &Path, to: &Path) -> io::Result<()> {
    let mut source = fs::File::open(from)?;
    let mut target = OpenOptions::new().write(true).create_new(true).open(to)?;
    let copied = io::copy(&mut source, &mut target)
        .and_then(|_| target.flush())
        .and_then(|()| {
            let permissions = source.metadata()?.permissions();
            fs::set_permissions(to, permissions)
        });
    if let Err(e) = copied {
        drop(target);
        let _ = fs::remove_file(to);
        return Err(e);
    }
    Ok(())
}

/// Copies the folder `from` to `to`, which must not exist. A link anywhere
/// inside refuses the copy before anything is made; a failure part way
/// removes what was made.
fn copy_dir_new(from: &Path, to: &Path, rel: &str) -> Result<(), FileOpError> {
    check_copyable(from, rel)?;
    fs::create_dir(to).map_err(|e| exists_or(e, rel, "copy it"))?;
    let copied = copy_tree(from, to);
    if let Err(e) = copied {
        // Only what this copy made: `to` did not exist before it.
        let _ = fs::remove_dir_all(to);
        return Err(io_error(format!("copy {rel}"))(e));
    }
    Ok(())
}

/// Refuses a folder that holds a link or folders more than
/// [`MAX_COPY_DEPTH`] levels down. Walked with a list, not recursion, so a
/// deep folder cannot overflow the thread's stack.
fn check_copyable(dir: &Path, rel: &str) -> Result<(), FileOpError> {
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    while let Some((at, depth)) = pending.pop() {
        let read = |e| io_error(format!("read {rel}"))(e);
        for entry in fs::read_dir(&at).map_err(read)? {
            let entry = entry.map_err(read)?;
            let kind = entry.file_type().map_err(read)?;
            if kind.is_symlink() {
                return Err(FileOpError::Link(shown(rel)));
            }
            if kind.is_dir() {
                if depth + 1 > MAX_COPY_DEPTH {
                    return Err(FileOpError::TooDeep(shown(rel)));
                }
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    Ok(())
}

/// Copies what `from` holds into the new folder `to`, with a list of
/// folders still to copy rather than recursion.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let mut pending = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((from, to)) = pending.pop() {
        for entry in fs::read_dir(&from)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let target = to.join(entry.file_name());
            if kind.is_symlink() {
                return Err(io::Error::other("a link appeared while copying"));
            } else if kind.is_dir() {
                fs::create_dir(&target)?;
                pending.push((entry.path(), target));
            } else {
                copy_file_new(&entry.path(), &target)?;
            }
        }
    }
    Ok(())
}

/// Deletes the entry for good: a link itself, never what it points at; a
/// folder with everything in it.
fn remove_entry(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTypeExt;
            // A directory link or junction goes as a folder, without its target.
            if meta.file_type().is_symlink_dir() {
                return fs::remove_dir(path);
            }
        }
        return fs::remove_file(path);
    }
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Moves `from` to `to`, failing with `AlreadyExists` when `to` is taken.
#[cfg(windows)]
fn move_new(from: &Path, to: &Path) -> io::Result<()> {
    windows::move_no_replace(from, to)
}

/// Moves `from` to `to`, failing with `AlreadyExists` when `to` is taken by
/// another entry. The check and the move are two steps here.
#[cfg(not(windows))]
fn move_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if let Ok(there) = fs::symlink_metadata(to) {
        let here = fs::symlink_metadata(from)?;
        // A name that differs only in case on a case-insensitive disk.
        if (there.dev(), there.ino()) != (here.dev(), here.ino()) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
    }
    fs::rename(from, to)
}

#[cfg(windows)]
fn move_to_trash(path: &Path) -> Result<(), FileOpError> {
    windows::move_to_recycle_bin(path).map_err(io_error("move it to the Recycle Bin"))
}

#[cfg(not(windows))]
fn move_to_trash(_: &Path) -> Result<(), FileOpError> {
    Err(FileOpError::Unsupported("Moving to the trash"))
}

#[cfg(windows)]
fn show_in_file_manager(path: &Path) -> Result<(), FileOpError> {
    windows::select_in_explorer(path).map_err(io_error("open File Explorer"))
}

#[cfg(not(windows))]
fn show_in_file_manager(path: &Path) -> Result<(), FileOpError> {
    let (program, target) = if cfg!(target_os = "macos") {
        ("open", vec!["-R".into(), path.as_os_str().to_owned()])
    } else {
        // No portable way to select an item: open the folder holding it.
        let parent = path.parent().unwrap_or(path);
        ("xdg-open", vec![parent.as_os_str().to_owned()])
    };
    let mut child = std::process::Command::new(program)
        .args(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(io_error(format!("start {program}")))?;
    // Reaps the child so it does not linger as a zombie; this thread is ours.
    let _ = child.wait();
    Ok(())
}

#[cfg(test)]
mod tests;
