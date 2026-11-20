use super::*;

struct Scratch {
    base: PathBuf,
    /// The tab's folder, inside `base`; `base` also holds what is outside it.
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("yien-file-ops-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(base.join("outside")).unwrap();
        fs::write(base.join("outside").join("keep.txt"), "outside").unwrap();
        Self { base, root }
    }

    fn file(&self, rel: &str, text: &str) {
        let path = resolve(&self.root, rel).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn dir(&self, rel: &str) {
        fs::create_dir_all(resolve(&self.root, rel).unwrap()).unwrap();
    }

    fn read(&self, rel: &str) -> Option<String> {
        fs::read_to_string(resolve(&self.root, rel).unwrap()).ok()
    }

    fn exists(&self, rel: &str) -> bool {
        fs::symlink_metadata(resolve(&self.root, rel).unwrap()).is_ok()
    }

    fn run(&self, action: FileAction) -> Result<Done, FileOpError> {
        run(&self.root, &action)
    }

    /// Every name under the tab's folder and the outside folder, sorted.
    fn snapshot(&self) -> Vec<String> {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
                let kind = entry.file_type().unwrap();
                out.push(name.clone());
                if kind.is_dir() && !kind.is_symlink() {
                    walk(&entry.path(), &format!("{name}/"), out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.base, "", &mut out);
        out.sort();
        out
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Links first, so removing the scratch never reaches a target.
        let _ = remove_links(&self.base);
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn remove_links(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            remove_entry(&entry.path())?;
        } else if kind.is_dir() {
            remove_links(&entry.path())?;
        }
    }
    Ok(())
}

fn new_file(dir: &str, name: &str) -> FileAction {
    FileAction::NewFile {
        dir: dir.into(),
        name: name.into(),
    }
}

fn rename(path: &str, name: &str) -> FileAction {
    FileAction::Rename {
        path: path.into(),
        name: name.into(),
    }
}

fn paste(from: &str, into: &str, cut: bool) -> FileAction {
    FileAction::Paste {
        from: from.into(),
        into: into.into(),
        cut,
    }
}

/// A junction (Windows) or a directory symlink at `link` to `target`.
#[cfg(windows)]
fn link_dir(target: &Path, link: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd")
        .arg("/c")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
fn link_dir(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[test]
fn rnf_14_new_files_and_folders_are_made_inside_and_never_overwrite() {
    let s = Scratch::new("new");
    s.dir("src");
    let done = s.run(new_file("src", "lib.rs")).unwrap();
    assert_eq!(done.dirs, ["src"]);
    assert_eq!(done.select.as_deref(), Some("src/lib.rs"));
    assert_eq!(s.read("src/lib.rs").as_deref(), Some(""));
    s.file("src/main.rs", "fn main() {}");
    assert!(matches!(
        s.run(new_file("src", "main.rs")),
        Err(FileOpError::Exists(_))
    ));
    assert_eq!(s.read("src/main.rs").as_deref(), Some("fn main() {}"));
    let done = s
        .run(FileAction::NewFolder {
            dir: String::new(),
            name: "docs".into(),
        })
        .unwrap();
    assert_eq!(done.dirs, [""]);
    assert!(fs::metadata(s.root.join("docs")).unwrap().is_dir());
    assert!(matches!(
        s.run(FileAction::NewFolder {
            dir: String::new(),
            name: "docs".into(),
        }),
        Err(FileOpError::Exists(_))
    ));
    assert!(matches!(
        s.run(new_file("", "a/b")),
        Err(FileOpError::BadName(_))
    ));
    assert!(matches!(
        s.run(new_file("absent", "x")),
        Err(FileOpError::Missing(_))
    ));
    assert!(matches!(
        s.run(new_file("src/main.rs", "x")),
        Err(FileOpError::NotAFolder(_))
    ));
}

#[test]
fn rnf_14_a_path_that_climbs_out_is_refused_and_changes_nothing() {
    let s = Scratch::new("climb");
    s.file("a.txt", "a");
    let before = s.snapshot();
    for action in [
        new_file("..", "x.txt"),
        new_file("../outside", "x.txt"),
        rename("../outside/keep.txt", "gone.txt"),
        FileAction::Delete {
            path: "../outside/keep.txt".into(),
        },
        FileAction::Trash {
            path: "../outside".into(),
        },
        paste("a.txt", "../outside", false),
        paste("../outside/keep.txt", "", true),
        FileAction::Reveal {
            path: "../outside".into(),
        },
    ] {
        assert!(
            matches!(s.run(action.clone()), Err(FileOpError::Outside(_))),
            "{action:?}"
        );
    }
    assert_eq!(s.snapshot(), before);
}

#[test]
fn rnf_14_a_link_out_of_the_folder_is_never_written_through() {
    let s = Scratch::new("link");
    s.file("a.txt", "a");
    if !link_dir(&s.base.join("outside"), &s.root.join("out")) {
        eprintln!("skipped: cannot create a directory link here");
        return;
    }
    let before = s.snapshot();
    for action in [
        new_file("out", "x.txt"),
        FileAction::NewFolder {
            dir: "out".into(),
            name: "x".into(),
        },
        rename("out/keep.txt", "gone.txt"),
        FileAction::Delete {
            path: "out/keep.txt".into(),
        },
        FileAction::Trash {
            path: "out/keep.txt".into(),
        },
        FileAction::Duplicate {
            path: "out/keep.txt".into(),
        },
        paste("a.txt", "out", false),
        paste("a.txt", "out", true),
        paste("out/keep.txt", "", false),
        paste("out/keep.txt", "", true),
        FileAction::Ignore {
            path: "out/keep.txt".into(),
        },
        FileAction::Reveal {
            path: "out/keep.txt".into(),
        },
    ] {
        assert!(
            matches!(s.run(action.clone()), Err(FileOpError::Outside(_))),
            "{action:?}"
        );
    }
    assert!(
        matches!(
            s.run(FileAction::Duplicate { path: "out".into() }),
            Err(FileOpError::Link(_))
        ),
        "a link is never copied"
    );
    assert_eq!(s.snapshot(), before);
    // The link itself lies inside: deleting it removes the link, not what it
    // points at.
    s.run(FileAction::Delete { path: "out".into() }).unwrap();
    assert!(!s.exists("out"));
    assert_eq!(
        fs::read_to_string(s.base.join("outside").join("keep.txt")).unwrap(),
        "outside"
    );
}

#[test]
fn rnf_14_nothing_is_written_inside_git() {
    let s = Scratch::new("git");
    s.file(".git/HEAD", "ref: refs/heads/main");
    s.file("a.txt", "a");
    let linked = link_dir(&s.root.join(".git"), &s.root.join("gitlink"));
    let before = s.snapshot();
    let mut refused = vec![
        new_file(".git", "x"),
        new_file("", ".git"),
        rename(".git/HEAD", "x"),
        rename(".git", "git"),
        rename("a.txt", ".git"),
        FileAction::Delete {
            path: ".git/HEAD".into(),
        },
        FileAction::Delete {
            path: ".GIT".into(),
        },
        FileAction::Trash {
            path: ".git".into(),
        },
        FileAction::Duplicate {
            path: ".git/HEAD".into(),
        },
        paste("a.txt", ".git", false),
        paste(".git/HEAD", "", true),
    ];
    if linked {
        refused.push(new_file("gitlink", "x"));
        refused.push(paste("a.txt", "gitlink", true));
    }
    for action in refused {
        let result = s.run(action.clone());
        assert!(
            matches!(
                result,
                Err(FileOpError::GitFolder | FileOpError::BadName(_) | FileOpError::Missing(_))
            ),
            "{action:?}: {result:?}"
        );
        if cfg!(windows) || !matches!(action, FileAction::Delete { ref path } if path == ".GIT") {
            assert!(
                !matches!(result, Err(FileOpError::Missing(_))),
                "{action:?}"
            );
        }
    }
    assert_eq!(s.snapshot(), before);
    // Reading .git is not writing: reveal and copying out are allowed.
    let done = s.run(paste(".git/HEAD", "", false)).unwrap();
    assert_eq!(done.select.as_deref(), Some("HEAD"));
}

#[test]
fn the_tab_folder_itself_is_never_renamed_moved_or_deleted() {
    let s = Scratch::new("root");
    s.dir("sub");
    for action in [
        rename("", "x"),
        FileAction::Delete {
            path: String::new(),
        },
        FileAction::Trash {
            path: String::new(),
        },
        FileAction::Ignore {
            path: String::new(),
        },
        paste("", "sub", true),
        paste("", "sub", false),
    ] {
        assert!(
            matches!(s.run(action.clone()), Err(FileOpError::Root(_))),
            "{action:?}"
        );
    }
    assert!(s.root.is_dir());
}

#[test]
fn rename_keeps_the_folder_and_never_replaces_another_entry() {
    let s = Scratch::new("rename");
    s.file("src/a.rs", "a");
    s.file("src/b.rs", "b");
    let done = s.run(rename("src/a.rs", "c.rs")).unwrap();
    assert_eq!(done.dirs, ["src"]);
    assert_eq!(done.select.as_deref(), Some("src/c.rs"));
    assert_eq!(s.read("src/c.rs").as_deref(), Some("a"));
    assert!(!s.exists("src/a.rs"));
    assert!(matches!(
        s.run(rename("src/c.rs", "b.rs")),
        Err(FileOpError::Exists(_))
    ));
    assert_eq!(s.read("src/b.rs").as_deref(), Some("b"));
    assert_eq!(s.read("src/c.rs").as_deref(), Some("a"));
    // The same name is no change; a name that differs only in case renames.
    assert!(s.run(rename("src/c.rs", "c.rs")).is_ok());
    s.run(rename("src/c.rs", "C.rs")).unwrap();
    let names: Vec<String> = fs::read_dir(s.root.join("src"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"C.rs".to_owned()), "{names:?}");
    s.dir("src/sub");
    s.run(rename("src/sub", "inner")).unwrap();
    assert!(s.root.join("src").join("inner").is_dir());
    assert!(matches!(
        s.run(rename("src/gone.rs", "x.rs")),
        Err(FileOpError::Missing(_))
    ));
}

#[test]
fn duplicate_counts_copies_up_and_copies_files_only() {
    let s = Scratch::new("duplicate");
    s.file("notes/a.txt", "text");
    let first = s
        .run(FileAction::Duplicate {
            path: "notes/a.txt".into(),
        })
        .unwrap();
    assert_eq!(first.select.as_deref(), Some("notes/a copy.txt"));
    let second = s
        .run(FileAction::Duplicate {
            path: "notes/a.txt".into(),
        })
        .unwrap();
    assert_eq!(second.select.as_deref(), Some("notes/a copy 2.txt"));
    assert_eq!(s.read("notes/a copy 2.txt").as_deref(), Some("text"));
    assert_eq!(s.read("notes/a.txt").as_deref(), Some("text"));
    assert!(matches!(
        s.run(FileAction::Duplicate {
            path: "notes".into()
        }),
        Err(FileOpError::NotAFile(_))
    ));
}

#[test]
fn paste_copies_or_moves_and_takes_a_copy_name_on_a_clash() {
    let s = Scratch::new("paste");
    s.file("a.txt", "a");
    s.file("dst/a.txt", "old");
    s.file("tree/x/y.txt", "y");
    let copied = s.run(paste("a.txt", "dst", false)).unwrap();
    assert_eq!(copied.select.as_deref(), Some("dst/a copy.txt"));
    assert_eq!(copied.dirs, ["dst"]);
    assert_eq!(s.read("dst/a.txt").as_deref(), Some("old"));
    assert_eq!(s.read("dst/a copy.txt").as_deref(), Some("a"));
    assert_eq!(s.read("a.txt").as_deref(), Some("a"));
    // A copy into the same folder is a duplicate.
    let again = s.run(paste("a.txt", "", false)).unwrap();
    assert_eq!(again.select.as_deref(), Some("a copy.txt"));
    let folder = s.run(paste("tree", "dst", false)).unwrap();
    assert_eq!(folder.select.as_deref(), Some("dst/tree"));
    assert_eq!(s.read("dst/tree/x/y.txt").as_deref(), Some("y"));
    let moved = s.run(paste("a.txt", "dst", true)).unwrap();
    assert_eq!(moved.select.as_deref(), Some("dst/a copy 2.txt"));
    assert_eq!(moved.dirs, ["dst", ""]);
    assert!(!s.exists("a.txt"));
    // Cut and pasted where it is: nothing to do.
    let stay = s.run(paste("dst/tree", "dst", true)).unwrap();
    assert!(stay.dirs.is_empty());
    assert!(s.exists("dst/tree/x/y.txt"));
}

#[test]
fn a_folder_never_goes_inside_itself() {
    let s = Scratch::new("itself");
    s.file("tree/x/y.txt", "y");
    let before = s.snapshot();
    for cut in [false, true] {
        for into in ["tree", "tree/x"] {
            assert!(
                matches!(
                    s.run(paste("tree", into, cut)),
                    Err(FileOpError::IntoItself(_))
                ),
                "{into} {cut}"
            );
        }
    }
    assert_eq!(s.snapshot(), before);
}

#[test]
fn a_folder_holding_a_link_is_not_copied_and_nothing_is_left_behind() {
    let s = Scratch::new("holds-link");
    s.file("tree/a.txt", "a");
    if !link_dir(&s.base.join("outside"), &s.root.join("tree").join("out")) {
        eprintln!("skipped: cannot create a directory link here");
        return;
    }
    let before = s.snapshot();
    assert!(matches!(
        s.run(paste("tree", "", false)),
        Err(FileOpError::Link(_))
    ));
    assert_eq!(s.snapshot(), before);
    // Moving it moves the link with it, not the target.
    s.dir("dst");
    s.run(paste("tree", "dst", true)).unwrap();
    assert!(s.exists("dst/tree/out"));
    assert!(s.base.join("outside").join("keep.txt").exists());
}

#[test]
fn delete_removes_a_file_or_a_whole_folder() {
    let s = Scratch::new("delete");
    s.file("a.txt", "a");
    s.file("tree/x/y.txt", "y");
    let done = s
        .run(FileAction::Delete {
            path: "a.txt".into(),
        })
        .unwrap();
    assert_eq!(done.dirs, [""]);
    assert!(!s.exists("a.txt"));
    s.run(FileAction::Delete {
        path: "tree/x".into(),
    })
    .unwrap();
    assert!(!s.exists("tree/x"));
    assert!(s.exists("tree"));
    assert!(matches!(
        s.run(FileAction::Delete {
            path: "a.txt".into()
        }),
        Err(FileOpError::Missing(_))
    ));
}

#[test]
fn ignore_adds_one_anchored_line_and_creates_the_file() {
    let s = Scratch::new("ignore");
    s.file("build/out.bin", "x");
    s.file("src/gen.rs", "x");
    let done = s
        .run(FileAction::Ignore {
            path: "build".into(),
        })
        .unwrap();
    assert_eq!(done.dirs, [""]);
    assert_eq!(s.read(".gitignore").as_deref(), Some("/build/\n"));
    s.run(FileAction::Ignore {
        path: "build".into(),
    })
    .unwrap();
    assert_eq!(s.read(".gitignore").as_deref(), Some("/build/\n"), "once");
    fs::write(s.root.join(".gitignore"), "/build/\r\n*.log").unwrap();
    let done = s
        .run(FileAction::Ignore {
            path: "src/gen.rs".into(),
        })
        .unwrap();
    assert_eq!(done.dirs, ["", "src"]);
    assert_eq!(
        s.read(".gitignore").as_deref(),
        Some("/build/\r\n*.log\r\n/src/gen.rs\r\n")
    );
    fs::write(s.root.join(".gitignore"), [0xff, 0xfe, 0x00]).unwrap();
    assert!(matches!(
        s.run(FileAction::Ignore {
            path: "src/gen.rs".into(),
        }),
        Err(FileOpError::BadGitignore)
    ));
}

#[test]
fn reveal_and_trash_refuse_before_reaching_the_shell() {
    let s = Scratch::new("shell");
    assert!(matches!(
        s.run(FileAction::Reveal {
            path: "absent".into()
        }),
        Err(FileOpError::Missing(_))
    ));
    assert!(matches!(
        s.run(FileAction::Trash {
            path: "absent".into()
        }),
        Err(FileOpError::Missing(_))
    ));
}
