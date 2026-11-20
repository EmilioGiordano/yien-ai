//! The Files view's context menu (RF-CTX-01), decided without drawing:
//! which items an entry offers, what each one does, and the file clipboard
//! behind Cut, Copy and Paste. The window draws the menu and runs the
//! [`FileEffect`] an item hands back; the disk is touched only by the core.

use yien_proto::file_ops::{FileAction, rel_parent};
use yien_proto::files::EntryKind;
use yien_proto::same_folder;

use crate::file_tree::join_relative;

/// One item of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileMenuAction {
    Preview,
    Edit,
    OpenDefault,
    OpenTerminal,
    Reveal,
    NewFile,
    NewFolder,
    Cut,
    Copy,
    Paste,
    Duplicate,
    CopyPath,
    CopyRelativePath,
    Rename,
    Trash,
    Delete,
    Ignore,
    CollapseAll,
}

/// The menu's groups, separated by a line, in VS Code's explorer order.
const GROUPS: &[&[FileMenuAction]] = {
    use FileMenuAction::*;
    &[
        &[Preview, Edit, OpenDefault],
        &[OpenTerminal, Reveal],
        &[NewFile, NewFolder],
        &[Cut, Copy, Paste, Duplicate],
        &[CopyPath, CopyRelativePath],
        &[Rename, Trash, Delete],
        &[Ignore, CollapseAll],
    ]
};

impl FileMenuAction {
    /// What the item says.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Preview => "Open Preview",
            Self::Edit => "Open in Editor",
            Self::OpenDefault => "Open in Default App",
            Self::OpenTerminal => "Open in Terminal",
            Self::Reveal => reveal_label(),
            Self::NewFile => "New File…",
            Self::NewFolder => "New Folder…",
            Self::Cut => "Cut",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::Duplicate => "Duplicate",
            Self::CopyPath => "Copy Path",
            Self::CopyRelativePath => "Copy Relative Path",
            Self::Rename => "Rename…",
            Self::Trash => "Move to Trash",
            Self::Delete => "Delete Permanently",
            Self::Ignore => "Add to .gitignore",
            Self::CollapseAll => "Collapse All",
        }
    }

    /// The palette's title for the same action on the Files view's cursor
    /// (RF-NAV-02).
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Preview => "files: open preview",
            Self::Edit => "files: open in editor",
            Self::OpenDefault => "files: open in default app",
            Self::OpenTerminal => "files: open in terminal",
            Self::Reveal => "files: reveal in file manager",
            Self::NewFile => "files: new file…",
            Self::NewFolder => "files: new folder…",
            Self::Cut => "files: cut",
            Self::Copy => "files: copy",
            Self::Paste => "files: paste",
            Self::Duplicate => "files: duplicate",
            Self::CopyPath => "files: copy path",
            Self::CopyRelativePath => "files: copy relative path",
            Self::Rename => "files: rename…",
            Self::Trash => "files: move to trash",
            Self::Delete => "files: delete permanently…",
            Self::Ignore => "files: add to .gitignore",
            Self::CollapseAll => "files: collapse all",
        }
    }
}

/// What the OS calls its file manager.
fn reveal_label() -> &'static str {
    if cfg!(windows) {
        "Reveal in File Explorer"
    } else if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Open Containing Folder"
    }
}

/// The entry a menu acts on: `""` with [`EntryKind::Dir`] for the tab's
/// folder itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileTarget {
    pub(crate) path: String,
    pub(crate) kind: EntryKind,
}

impl FileTarget {
    /// The tab's folder itself.
    pub(crate) fn root() -> Self {
        Self {
            path: String::new(),
            kind: EntryKind::Dir,
        }
    }

    fn is_root(&self) -> bool {
        self.path.is_empty()
    }

    fn is_file(&self) -> bool {
        !self.kind.is_dir()
    }

    /// The folder New File, New Folder, Paste and Open in Terminal use: the
    /// entry when it is a folder, else the folder holding it.
    pub(crate) fn folder(&self) -> String {
        if self.kind.is_dir() {
            self.path.clone()
        } else {
            rel_parent(&self.path).to_owned()
        }
    }
}

/// What Cut or Copy took, from which tab folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileClip {
    pub(crate) root: String,
    pub(crate) path: String,
    pub(crate) cut: bool,
}

/// What the menu knows besides the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct MenuFacts {
    /// The tab's folder is in a git repository.
    pub(crate) git: bool,
    /// The clipboard holds an entry of this tab's folder.
    pub(crate) can_paste: bool,
}

impl MenuFacts {
    /// The facts for a tab folder `root`, with `clip` on the clipboard.
    pub(crate) fn new(git: bool, clip: Option<&FileClip>, root: &str) -> Self {
        Self {
            git,
            can_paste: clip.is_some_and(|c| same_folder(&c.root, root)),
        }
    }
}

/// Whether the menu of `target` shows `action` at all.
fn offered(action: FileMenuAction, target: &FileTarget, facts: MenuFacts) -> bool {
    use FileMenuAction::*;
    match action {
        Preview | Edit | OpenDefault | Duplicate => target.is_file(),
        Cut | Copy | CopyRelativePath | Rename | Trash | Delete => !target.is_root(),
        Ignore => facts.git && !target.is_root(),
        CollapseAll => target.kind.is_dir(),
        OpenTerminal | Reveal | NewFile | NewFolder | Paste | CopyPath => true,
    }
}

/// Whether a shown `action` can run now.
fn enabled(action: FileMenuAction, facts: MenuFacts) -> bool {
    action != FileMenuAction::Paste || facts.can_paste
}

/// One line of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileMenuEntry {
    Item {
        action: FileMenuAction,
        enabled: bool,
    },
    Separator,
}

/// The menu of `target`: the items it offers, in groups.
pub(crate) fn file_menu(target: &FileTarget, facts: MenuFacts) -> Vec<FileMenuEntry> {
    let mut entries = Vec::new();
    for group in GROUPS {
        let items: Vec<FileMenuEntry> = group
            .iter()
            .filter(|a| offered(**a, target, facts))
            .map(|a| FileMenuEntry::Item {
                action: *a,
                enabled: enabled(*a, facts),
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        if !entries.is_empty() {
            entries.push(FileMenuEntry::Separator);
        }
        entries.extend(items);
    }
    entries
}

/// The actions the palette offers for `target`: those its menu shows that
/// can run now.
pub(crate) fn palette_actions(target: &FileTarget, facts: MenuFacts) -> Vec<FileMenuAction> {
    file_menu(target, facts)
        .into_iter()
        .filter_map(|e| match e {
            FileMenuEntry::Item {
                action,
                enabled: true,
            } => Some(action),
            _ => None,
        })
        .collect()
}

/// What running an item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileEffect {
    /// Show the file in the preview pane (RF-CTX-02).
    Preview(String),
    /// Open the file in the configured editor (RF-CTX-03).
    Edit(String),
    /// Open the file in the OS default app.
    OpenDefault(String),
    /// Open a tab with the shell started in this folder.
    Terminal(String),
    /// Ask the core.
    Run(FileAction),
    /// Open the name field for a new file or folder in `dir`.
    New { dir: String, folder: bool },
    /// Open the name field on the entry.
    Rename(String),
    /// Ask before deleting the entry for good.
    AskDelete(String),
    /// Put the entry on the file clipboard.
    Clip(FileClip),
    /// Put this text on the system clipboard.
    CopyText(String),
    /// Fold every folder of the tree.
    CollapseAll,
}

/// What `action` does to `target` in the tab folder `root`, with `clip` on
/// the file clipboard; `None` when the menu would not offer it or it cannot
/// run now.
pub(crate) fn file_effect(
    action: FileMenuAction,
    target: &FileTarget,
    facts: MenuFacts,
    root: &str,
    clip: Option<&FileClip>,
) -> Option<FileEffect> {
    use FileMenuAction::*;
    if !offered(action, target, facts) || !enabled(action, facts) {
        return None;
    }
    let path = target.path.clone();
    Some(match action {
        Preview => FileEffect::Preview(path),
        Edit => FileEffect::Edit(path),
        OpenDefault => FileEffect::OpenDefault(path),
        OpenTerminal => FileEffect::Terminal(target.folder()),
        Reveal => FileEffect::Run(FileAction::Reveal { path }),
        NewFile | NewFolder => FileEffect::New {
            dir: target.folder(),
            folder: action == NewFolder,
        },
        Cut | Copy => FileEffect::Clip(FileClip {
            root: root.to_owned(),
            path,
            cut: action == Cut,
        }),
        Paste => {
            let clip = clip?;
            FileEffect::Run(FileAction::Paste {
                from: clip.path.clone(),
                into: target.folder(),
                cut: clip.cut,
            })
        }
        Duplicate => FileEffect::Run(FileAction::Duplicate { path }),
        CopyPath => {
            FileEffect::CopyText(join_relative(root, &path)?.to_string_lossy().into_owned())
        }
        CopyRelativePath => FileEffect::CopyText(relative_text(&path, cfg!(windows))),
        Rename => FileEffect::Rename(path),
        Trash => FileEffect::Run(FileAction::Trash { path }),
        Delete => FileEffect::AskDelete(path),
        Ignore => FileEffect::Run(FileAction::Ignore { path }),
        CollapseAll => FileEffect::CollapseAll,
    })
}

/// A relative path as the system writes it: `\` between parts on Windows.
fn relative_text(rel: &str, windows: bool) -> String {
    if windows {
        rel.replace('/', "\\")
    } else {
        rel.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FileMenuAction::*;

    fn file(path: &str) -> FileTarget {
        FileTarget {
            path: path.into(),
            kind: EntryKind::File,
        }
    }

    fn folder(path: &str) -> FileTarget {
        FileTarget {
            path: path.into(),
            kind: EntryKind::Dir,
        }
    }

    fn items(target: &FileTarget, facts: MenuFacts) -> Vec<(FileMenuAction, bool)> {
        file_menu(target, facts)
            .into_iter()
            .filter_map(|e| match e {
                FileMenuEntry::Item { action, enabled } => Some((action, enabled)),
                FileMenuEntry::Separator => None,
            })
            .collect()
    }

    fn actions(target: &FileTarget, facts: MenuFacts) -> Vec<FileMenuAction> {
        items(target, facts).into_iter().map(|(a, _)| a).collect()
    }

    const GIT: MenuFacts = MenuFacts {
        git: true,
        can_paste: true,
    };

    #[test]
    fn rf_ctx_01_a_file_offers_opening_editing_and_its_file_actions() {
        assert_eq!(
            actions(&file("src/main.rs"), GIT),
            [
                Preview,
                Edit,
                OpenDefault,
                OpenTerminal,
                Reveal,
                NewFile,
                NewFolder,
                Cut,
                Copy,
                Paste,
                Duplicate,
                CopyPath,
                CopyRelativePath,
                Rename,
                Trash,
                Delete,
                Ignore
            ]
        );
    }

    #[test]
    fn rf_ctx_01_a_folder_has_no_open_or_duplicate_and_can_collapse_all() {
        let shown = actions(&folder("src"), GIT);
        for missing in [Preview, Edit, OpenDefault, Duplicate] {
            assert!(!shown.contains(&missing), "{missing:?}");
        }
        assert!(shown.contains(&CollapseAll));
        assert!(!actions(&file("a"), GIT).contains(&CollapseAll));
        // A link to a folder is a folder here; a link to a file is a file.
        let dir_link = FileTarget {
            path: "l".into(),
            kind: EntryKind::Link { dir: true },
        };
        assert!(actions(&dir_link, GIT).contains(&CollapseAll));
        let file_link = FileTarget {
            path: "l".into(),
            kind: EntryKind::Link { dir: false },
        };
        assert!(actions(&file_link, GIT).contains(&Duplicate));
    }

    #[test]
    fn rnf_14_the_tab_folder_itself_is_never_offered_to_move_rename_or_delete() {
        let shown = actions(&FileTarget::root(), GIT);
        for missing in [
            Cut,
            Copy,
            Rename,
            Trash,
            Delete,
            Ignore,
            CopyRelativePath,
            Duplicate,
        ] {
            assert!(!shown.contains(&missing), "{missing:?}");
        }
        assert_eq!(
            shown,
            [
                OpenTerminal,
                Reveal,
                NewFile,
                NewFolder,
                Paste,
                CopyPath,
                CollapseAll
            ]
        );
        for action in [Rename, Trash, Delete, Cut] {
            assert_eq!(
                file_effect(action, &FileTarget::root(), GIT, "C:/p", None),
                None
            );
        }
    }

    #[test]
    fn rf_ctx_01_add_to_gitignore_shows_only_in_a_repository() {
        let plain = MenuFacts {
            git: false,
            can_paste: false,
        };
        assert!(!actions(&file("a"), plain).contains(&Ignore));
        assert!(actions(&file("a"), GIT).contains(&Ignore));
        assert_eq!(file_effect(Ignore, &file("a"), plain, "C:/p", None), None);
    }

    #[test]
    fn rf_ctx_01_paste_waits_for_something_cut_or_copied_in_this_folder() {
        let clip = FileClip {
            root: "C:/p".into(),
            path: "a.txt".into(),
            cut: true,
        };
        let here = MenuFacts::new(true, Some(&clip), "C:/p/");
        assert!(here.can_paste, "the same folder, spelled another way");
        let elsewhere = MenuFacts::new(true, Some(&clip), "C:/other");
        assert!(!elsewhere.can_paste);
        assert!(!MenuFacts::new(true, None, "C:/p").can_paste);
        let paste = |facts| {
            items(&file("src/b.rs"), facts)
                .into_iter()
                .find(|(a, _)| *a == Paste)
                .unwrap()
                .1
        };
        assert!(paste(here));
        assert!(!paste(elsewhere), "shown, but off");
        assert_eq!(
            file_effect(Paste, &file("src/b.rs"), here, "C:/p", Some(&clip)),
            Some(FileEffect::Run(FileAction::Paste {
                from: "a.txt".into(),
                into: "src".into(),
                cut: true
            }))
        );
        assert_eq!(
            file_effect(Paste, &file("src/b.rs"), elsewhere, "C:/other", Some(&clip)),
            None
        );
        // The palette lists only what can run.
        assert!(!palette_actions(&file("x"), elsewhere).contains(&Paste));
        assert!(palette_actions(&file("x"), here).contains(&Paste));
    }

    #[test]
    fn rf_ctx_01_new_entries_and_terminals_go_into_the_folder_or_beside_the_file() {
        let new = file_effect(NewFile, &file("src/ui/a.rs"), GIT, "C:/p", None);
        assert_eq!(
            new,
            Some(FileEffect::New {
                dir: "src/ui".into(),
                folder: false
            })
        );
        let new = file_effect(NewFolder, &folder("src"), GIT, "C:/p", None);
        assert_eq!(
            new,
            Some(FileEffect::New {
                dir: "src".into(),
                folder: true
            })
        );
        assert_eq!(
            file_effect(OpenTerminal, &file("README.md"), GIT, "C:/p", None),
            Some(FileEffect::Terminal(String::new()))
        );
        assert_eq!(
            file_effect(OpenTerminal, &folder("crates/x"), GIT, "C:/p", None),
            Some(FileEffect::Terminal("crates/x".into()))
        );
    }

    #[test]
    fn rf_ctx_01_each_item_does_what_it_says_on_the_entry_it_was_opened_on() {
        let target = file("src/main.rs");
        let run = |action| file_effect(action, &target, GIT, "C:/p", None);
        assert_eq!(
            run(Preview),
            Some(FileEffect::Preview("src/main.rs".into()))
        );
        assert_eq!(run(Edit), Some(FileEffect::Edit("src/main.rs".into())));
        assert_eq!(
            run(OpenDefault),
            Some(FileEffect::OpenDefault("src/main.rs".into()))
        );
        assert_eq!(
            run(Reveal),
            Some(FileEffect::Run(FileAction::Reveal {
                path: "src/main.rs".into()
            }))
        );
        assert_eq!(
            run(Duplicate),
            Some(FileEffect::Run(FileAction::Duplicate {
                path: "src/main.rs".into()
            }))
        );
        assert_eq!(
            run(Trash),
            Some(FileEffect::Run(FileAction::Trash {
                path: "src/main.rs".into()
            }))
        );
        assert_eq!(
            run(Delete),
            Some(FileEffect::AskDelete("src/main.rs".into())),
            "never deleted without asking"
        );
        assert_eq!(
            run(Ignore),
            Some(FileEffect::Run(FileAction::Ignore {
                path: "src/main.rs".into()
            }))
        );
        assert_eq!(run(Rename), Some(FileEffect::Rename("src/main.rs".into())));
        assert_eq!(
            run(Cut),
            Some(FileEffect::Clip(FileClip {
                root: "C:/p".into(),
                path: "src/main.rs".into(),
                cut: true
            }))
        );
        assert_eq!(
            run(Copy),
            Some(FileEffect::Clip(FileClip {
                root: "C:/p".into(),
                path: "src/main.rs".into(),
                cut: false
            }))
        );
        assert_eq!(run(CollapseAll), None, "a file has no Collapse All");
    }

    #[test]
    fn rf_ctx_01_copy_path_gives_the_full_path_and_relative_the_system_separators() {
        let target = file("src/main.rs");
        let full: std::path::PathBuf = ["C:/p", "src", "main.rs"].iter().collect();
        assert_eq!(
            file_effect(CopyPath, &target, GIT, "C:/p", None),
            Some(FileEffect::CopyText(full.to_string_lossy().into_owned()))
        );
        assert_eq!(relative_text("src/main.rs", true), "src\\main.rs");
        assert_eq!(relative_text("src/main.rs", false), "src/main.rs");
        assert_eq!(
            file_effect(CopyRelativePath, &target, GIT, "C:/p", None),
            Some(FileEffect::CopyText(relative_text(
                "src/main.rs",
                cfg!(windows)
            )))
        );
    }

    #[test]
    fn groups_are_separated_once_and_never_start_or_end_with_a_line() {
        for target in [file("a"), folder("b"), FileTarget::root()] {
            for git in [false, true] {
                let menu = file_menu(
                    &target,
                    MenuFacts {
                        git,
                        can_paste: false,
                    },
                );
                assert!(matches!(menu.first(), Some(FileMenuEntry::Item { .. })));
                assert!(matches!(menu.last(), Some(FileMenuEntry::Item { .. })));
                for pair in menu.windows(2) {
                    assert!(
                        !(pair[0] == FileMenuEntry::Separator
                            && pair[1] == FileMenuEntry::Separator)
                    );
                }
            }
        }
    }

    #[test]
    fn rf_nav_02_every_item_has_a_label_and_a_palette_title_of_its_own() {
        let all: Vec<FileMenuAction> = GROUPS.iter().flat_map(|g| g.iter().copied()).collect();
        assert_eq!(all.len(), 18);
        let mut labels = std::collections::HashSet::new();
        let mut titles = std::collections::HashSet::new();
        for action in all {
            assert!(labels.insert(action.label()), "{action:?}");
            assert!(titles.insert(action.title()), "{action:?}");
            assert!(action.title().starts_with("files: "), "{action:?}");
        }
    }
}
