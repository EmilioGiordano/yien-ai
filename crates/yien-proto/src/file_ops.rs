//! What the Files view's context menu asks the core to do to the files of a
//! tab's folder, and the pure rules those actions follow: which names are
//! valid, the free name a copy takes, the line `.gitignore` gets. Paths are
//! relative to the tab's folder, parts joined with `/`, as the file tree
//! gives them; the platform layer resolves them and refuses any that leaves
//! the folder (RNF-14).
//!
//! ```
//! use yien_proto::file_ops::{FileAction, copy_name};
//!
//! let duplicate = FileAction::Duplicate { path: "src/main.rs".into() };
//! assert!(duplicate.writes());
//! assert_eq!(copy_name("main.rs", false, 2), "main copy 2.rs");
//! ```

use serde::{Deserialize, Serialize};

/// The most copies [`free_copy_name`] tries before giving up.
pub const MAX_COPIES: u32 = 10_000;

/// The longest name a new or renamed entry may have, in bytes.
pub const MAX_NAME_LEN: usize = 255;

/// One action on the files of a tab's folder.
///
/// ```
/// use yien_proto::file_ops::FileAction;
///
/// let reveal = FileAction::Reveal { path: "README.md".into() };
/// assert!(!reveal.writes());
/// assert_eq!(reveal.kind(), "reveal");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileAction {
    /// Creates an empty file `name` in the folder `dir` (`""` for the root).
    NewFile {
        /// The folder.
        dir: String,
        /// The new file's name.
        name: String,
    },
    /// Creates a folder `name` in the folder `dir`.
    NewFolder {
        /// The folder.
        dir: String,
        /// The new folder's name.
        name: String,
    },
    /// Gives the entry `path` the name `name`, in the same folder.
    Rename {
        /// The entry.
        path: String,
        /// Its new name.
        name: String,
    },
    /// Copies the file `path` beside it as `name copy.ext`, or the first
    /// free `name copy N.ext`.
    Duplicate {
        /// The file.
        path: String,
    },
    /// Copies or moves the entry `from` into the folder `into`; a name
    /// already there gets ` copy` as with [`FileAction::Duplicate`].
    Paste {
        /// What was cut or copied.
        from: String,
        /// The folder it goes into.
        into: String,
        /// Move it instead of copying it.
        cut: bool,
    },
    /// Moves the entry to the OS recycle bin.
    Trash {
        /// The entry.
        path: String,
    },
    /// Deletes the entry for good; the UI asks first.
    Delete {
        /// The entry.
        path: String,
    },
    /// Appends the entry to the `.gitignore` of the tab's folder, creating
    /// the file when it is missing.
    Ignore {
        /// The entry.
        path: String,
    },
    /// Shows the entry selected in the OS file manager.
    Reveal {
        /// The entry.
        path: String,
    },
}

impl FileAction {
    /// The action's name, for logs: never a path.
    ///
    /// ```
    /// use yien_proto::file_ops::FileAction;
    /// assert_eq!(FileAction::Trash { path: "a".into() }.kind(), "trash");
    /// ```
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NewFile { .. } => "new_file",
            Self::NewFolder { .. } => "new_folder",
            Self::Rename { .. } => "rename",
            Self::Duplicate { .. } => "duplicate",
            Self::Paste { cut: false, .. } => "paste_copy",
            Self::Paste { cut: true, .. } => "paste_move",
            Self::Trash { .. } => "trash",
            Self::Delete { .. } => "delete",
            Self::Ignore { .. } => "ignore",
            Self::Reveal { .. } => "reveal",
        }
    }

    /// Whether it changes anything on disk.
    ///
    /// ```
    /// use yien_proto::file_ops::FileAction;
    /// assert!(FileAction::Delete { path: "a".into() }.writes());
    /// ```
    pub fn writes(&self) -> bool {
        !matches!(self, Self::Reveal { .. })
    }
}

/// The folder holding `rel`: `""` for an entry of the root.
///
/// ```
/// use yien_proto::file_ops::rel_parent;
/// assert_eq!(rel_parent("src/ui/main.rs"), "src/ui");
/// assert_eq!(rel_parent("README.md"), "");
/// ```
pub fn rel_parent(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The last part of `rel`.
///
/// ```
/// assert_eq!(yien_proto::file_ops::rel_name("src/main.rs"), "main.rs");
/// ```
pub fn rel_name(rel: &str) -> &str {
    rel.rsplit_once('/').map_or(rel, |(_, name)| name)
}

/// `name` inside the folder `dir` (`""` for the root).
///
/// ```
/// use yien_proto::file_ops::rel_join;
/// assert_eq!(rel_join("", "a.txt"), "a.txt");
/// assert_eq!(rel_join("src", "a.txt"), "src/a.txt");
/// ```
pub fn rel_join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// Whether a part of `rel` is `.git`, which no file action may write to;
/// on Windows (`windows`) names ignore case.
///
/// ```
/// use yien_proto::file_ops::is_git_path;
/// assert!(is_git_path(".git/config", false));
/// assert!(is_git_path("sub/.GIT", true));
/// assert!(!is_git_path(".github/flow.yml", true));
/// ```
pub fn is_git_path(rel: &str, windows: bool) -> bool {
    rel.split('/').any(|part| is_git_name(part, windows))
}

fn is_git_name(name: &str, windows: bool) -> bool {
    if windows {
        name.eq_ignore_ascii_case(".git")
    } else {
        name == ".git"
    }
}

/// Names Windows keeps for devices, with or without an extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Why `name` cannot be a new entry's name, for the user; `Ok` when it can.
/// With `windows`, the characters and names Windows refuses are refused too.
///
/// ```
/// use yien_proto::file_ops::check_name;
///
/// assert!(check_name("notes.md", true).is_ok());
/// assert!(check_name("a/b", false).is_err());
/// assert!(check_name("aux.txt", true).is_err());
/// assert!(check_name("aux.txt", false).is_ok());
/// ```
pub fn check_name(name: &str, windows: bool) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Type a name.".to_owned());
    }
    if name == "." || name == ".." {
        return Err(format!("{name} cannot be a name."));
    }
    if name.contains('/') || (windows && name.contains('\\')) {
        return Err("A name cannot contain / or \\.".to_owned());
    }
    if name.chars().any(char::is_control) {
        return Err("A name cannot contain control characters.".to_owned());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!("A name can be at most {MAX_NAME_LEN} bytes long."));
    }
    if is_git_name(name, windows) {
        return Err("YIEN does not create or rename anything to .git.".to_owned());
    }
    if windows {
        if name.contains(['<', '>', ':', '"', '|', '?', '*']) {
            return Err("A name cannot contain < > : \" | ? or *.".to_owned());
        }
        if name.ends_with(['.', ' ']) {
            return Err("A name cannot end with a space or a dot.".to_owned());
        }
        let stem = name.split('.').next().unwrap_or(name).trim_end();
        if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
            return Err(format!("Windows keeps the name {stem} for a device."));
        }
    }
    Ok(())
}

/// The `n`th copy's name of `name`: `name copy.ext` for the first, then
/// `name copy N.ext`. A folder, a name starting with its only dot
/// (`.gitignore`) or one without a dot gets the suffix at the end.
///
/// ```
/// use yien_proto::file_ops::copy_name;
///
/// assert_eq!(copy_name("main.rs", false, 1), "main copy.rs");
/// assert_eq!(copy_name("archive.tar.gz", false, 3), "archive.tar copy 3.gz");
/// assert_eq!(copy_name(".gitignore", false, 1), ".gitignore copy");
/// assert_eq!(copy_name("v1.2", true, 1), "v1.2 copy");
/// ```
pub fn copy_name(name: &str, is_dir: bool, n: u32) -> String {
    let suffix = if n <= 1 {
        " copy".to_owned()
    } else {
        format!(" copy {n}")
    };
    match name.rfind('.') {
        Some(dot) if !is_dir && dot > 0 => {
            format!("{}{suffix}{}", &name[..dot], &name[dot..])
        }
        _ => format!("{name}{suffix}"),
    }
}

/// The first copy name of `name` that `taken` says is free, `None` after
/// [`MAX_COPIES`] tries.
///
/// ```
/// use yien_proto::file_ops::free_copy_name;
///
/// let taken = |n: &str| n == "a copy.txt";
/// assert_eq!(free_copy_name("a.txt", false, taken).as_deref(), Some("a copy 2.txt"));
/// ```
pub fn free_copy_name(name: &str, is_dir: bool, taken: impl Fn(&str) -> bool) -> Option<String> {
    (1..=MAX_COPIES)
        .map(|n| copy_name(name, is_dir, n))
        .find(|candidate| !taken(candidate))
}

/// The name a pasted entry takes: its own when free, else the first free
/// copy name.
///
/// ```
/// use yien_proto::file_ops::paste_name;
///
/// assert_eq!(paste_name("a.txt", false, |_| false).as_deref(), Some("a.txt"));
/// assert_eq!(paste_name("a.txt", false, |n| n == "a.txt").as_deref(), Some("a copy.txt"));
/// ```
pub fn paste_name(name: &str, is_dir: bool, taken: impl Fn(&str) -> bool) -> Option<String> {
    if taken(name) {
        free_copy_name(name, is_dir, taken)
    } else {
        Some(name.to_owned())
    }
}

/// The `.gitignore` line that ignores exactly the entry `rel` of the folder
/// holding that `.gitignore`: anchored with a leading `/`, a folder with a
/// trailing `/`, and the characters git reads as a pattern escaped.
///
/// ```
/// use yien_proto::file_ops::gitignore_entry;
///
/// assert_eq!(gitignore_entry("target", true), "/target/");
/// assert_eq!(gitignore_entry("logs/a*.log", false), "/logs/a\\*.log");
/// ```
pub fn gitignore_entry(rel: &str, is_dir: bool) -> String {
    let mut line = String::with_capacity(rel.len() + 2);
    line.push('/');
    let chars: Vec<char> = rel.chars().collect();
    let trailing = chars.iter().rev().take_while(|c| **c == ' ').count();
    for (at, ch) in chars.iter().enumerate() {
        let escape = matches!(ch, '*' | '?' | '[' | ']' | '\\')
            || (*ch == ' ' && at >= chars.len() - trailing);
        if escape {
            line.push('\\');
        }
        line.push(*ch);
    }
    if is_dir {
        line.push('/');
    }
    line
}

/// What to append to a `.gitignore` holding `content` so it has the line
/// `entry`: the line, after a newline when the file does not end with one.
/// `None` when the line is there already.
///
/// ```
/// use yien_proto::file_ops::gitignore_addition;
///
/// assert_eq!(gitignore_addition("", "/a").as_deref(), Some("/a\n"));
/// assert_eq!(gitignore_addition("/b", "/a").as_deref(), Some("\n/a\n"));
/// assert_eq!(gitignore_addition("/a\r\n", "/a"), None);
/// ```
pub fn gitignore_addition(content: &str, entry: &str) -> Option<String> {
    if content
        .lines()
        .any(|line| line.trim_end_matches('\r') == entry)
    {
        return None;
    }
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let lead = if content.is_empty() || content.ends_with('\n') {
        ""
    } else {
        newline
    };
    Some(format!("{lead}{entry}{newline}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rnf_14_a_git_folder_is_found_in_any_part_of_a_path() {
        for rel in [".git", ".git/config", "sub/.git/HEAD", "a/.git"] {
            assert!(is_git_path(rel, false), "{rel}");
        }
        assert!(is_git_path("x/.Git/y", true));
        assert!(!is_git_path("x/.Git/y", false));
        for rel in ["", ".github", ".gitignore", "a.git", "git", ".claude/x"] {
            assert!(!is_git_path(rel, true), "{rel}");
        }
    }

    #[test]
    fn names_with_separators_dots_or_device_names_are_refused() {
        for bad in ["", "  ", ".", "..", "a/b", "a\u{7}", ".git"] {
            assert!(check_name(bad, false).is_err(), "{bad:?}");
            assert!(check_name(bad, true).is_err(), "{bad:?}");
        }
        for bad in [
            "a\\b", "a:b", "a?", "a*", "a\"", "a<b", "a|b", "trail.", "trail ", "CON", "nul.txt",
            "Com1", "lpt9.log", ".GIT",
        ] {
            assert!(check_name(bad, true).is_err(), "{bad:?}");
        }
        for unix_only in ["a\\b", "a:b", "trail.", "CON", ".GIT"] {
            assert!(check_name(unix_only, false).is_ok(), "{unix_only:?}");
        }
        for good in [
            "main.rs",
            ".gitignore",
            " lead",
            "console.log",
            "COM10",
            "a b",
            "ñandú.txt",
        ] {
            assert!(check_name(good, true).is_ok(), "{good:?}");
        }
        assert!(check_name(&"a".repeat(MAX_NAME_LEN + 1), false).is_err());
        assert!(check_name(&"a".repeat(MAX_NAME_LEN), false).is_ok());
    }

    #[test]
    fn copies_count_up_and_never_take_a_name_in_use() {
        assert_eq!(copy_name("a.txt", false, 1), "a copy.txt");
        assert_eq!(copy_name("a.txt", false, 2), "a copy 2.txt");
        assert_eq!(copy_name("Makefile", false, 1), "Makefile copy");
        assert_eq!(copy_name("src", true, 2), "src copy 2");
        let used = ["a.txt", "a copy.txt", "a copy 2.txt"];
        assert_eq!(
            free_copy_name("a.txt", false, |n| used.contains(&n)).as_deref(),
            Some("a copy 3.txt")
        );
        assert_eq!(free_copy_name("a.txt", false, |_| true), None);
        assert_eq!(
            paste_name("a.txt", false, |n| used.contains(&n)).as_deref(),
            Some("a copy 3.txt")
        );
        assert_eq!(
            paste_name("b.txt", false, |n| used.contains(&n)).as_deref(),
            Some("b.txt")
        );
    }

    #[test]
    fn a_gitignore_line_matches_only_its_entry() {
        assert_eq!(gitignore_entry("a.txt", false), "/a.txt");
        assert_eq!(gitignore_entry("a b/c[1]?", false), "/a b/c\\[1\\]\\?");
        assert_eq!(gitignore_entry("#notes", false), "/#notes");
        assert_eq!(gitignore_entry("!keep", true), "/!keep/");
        assert_eq!(gitignore_entry("x ", false), "/x\\ ");
        assert_eq!(gitignore_entry("back\\slash", false), "/back\\\\slash");
    }

    #[test]
    fn a_gitignore_gets_the_line_once_with_its_own_newlines() {
        assert_eq!(gitignore_addition("/a\n", "/b").as_deref(), Some("/b\n"));
        assert_eq!(
            gitignore_addition("/a\r\n/c", "/b").as_deref(),
            Some("\r\n/b\r\n")
        );
        assert_eq!(gitignore_addition("x\n/b\n", "/b"), None);
        assert_eq!(
            gitignore_addition("/bb\n", "/b").as_deref(),
            Some("/b\n"),
            "a longer line is another entry"
        );
    }

    #[test]
    fn relative_paths_split_and_join() {
        assert_eq!(rel_parent("a"), "");
        assert_eq!(rel_parent("a/b/c"), "a/b");
        assert_eq!(rel_name("a/b/c"), "c");
        assert_eq!(rel_name("c"), "c");
        assert_eq!(rel_join("a/b", "c"), "a/b/c");
    }

    #[test]
    fn only_reveal_leaves_the_disk_alone_and_every_kind_is_distinct() {
        let all = [
            FileAction::NewFile {
                dir: "d".into(),
                name: "n".into(),
            },
            FileAction::NewFolder {
                dir: "d".into(),
                name: "n".into(),
            },
            FileAction::Rename {
                path: "p".into(),
                name: "n".into(),
            },
            FileAction::Duplicate { path: "p".into() },
            FileAction::Paste {
                from: "p".into(),
                into: "d".into(),
                cut: true,
            },
            FileAction::Paste {
                from: "p".into(),
                into: "d".into(),
                cut: false,
            },
            FileAction::Trash { path: "p".into() },
            FileAction::Delete { path: "p".into() },
            FileAction::Ignore { path: "p".into() },
        ];
        let mut kinds = std::collections::HashSet::new();
        for action in &all {
            assert!(action.writes(), "{action:?}");
            assert!(kinds.insert(action.kind()), "{} twice", action.kind());
        }
        assert!(!FileAction::Reveal { path: "p".into() }.writes());
    }
}
