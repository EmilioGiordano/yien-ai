//! Folder paths as keys, so the UI and the core agree on when two spellings
//! name the same project folder (RF-ESP-01).

/// A folder path in a form fit for comparison, without repeated or trailing
/// separators (except a root's). With `windows`, it also drops a `\\?\`
/// prefix (`\\?\UNC\` becomes `//`), uses `/` as the only separator, trims
/// surrounding spaces and folds case, as Windows paths allow; elsewhere `\`
/// and spaces are ordinary characters of a name. It does not touch the disk,
/// so links and `..` are compared as written.
///
/// ```
/// use yien_proto::folder_key;
///
/// assert_eq!(folder_key(r"\\?\C:\Code\Yien\", true), "c:/code/yien");
/// assert_eq!(folder_key(r"\\?\UNC\server\share", true), "//server/share");
/// assert_eq!(folder_key("/home/e//proj/", false), "/home/e/proj");
/// assert_eq!(folder_key(r"/home/e/a\b", false), r"/home/e/a\b");
/// ```
pub fn folder_key(path: &str, windows: bool) -> String {
    let path = if windows {
        windows_prefixless(path.trim())
    } else {
        path.to_owned()
    };
    let mut key = String::with_capacity(path.len());
    for ch in path.chars() {
        let ch = if windows && ch == '\\' { '/' } else { ch };
        // A leading `//` is a UNC share and keeps both slashes.
        if ch == '/' && key.ends_with('/') && key.len() > 1 {
            continue;
        }
        key.push(ch);
    }
    while key.len() > 1 && key.ends_with('/') && !key.ends_with(":/") && key != "//" {
        key.pop();
    }
    if windows {
        key = key.to_lowercase();
    }
    key
}

/// A Windows path without its `\\?\` prefix; `\\?\UNC\server` is `\\server`.
fn windows_prefixless(path: &str) -> String {
    for unc in [r"\\?\UNC\", "//?/UNC/", r"\\?\unc\", "//?/unc/"] {
        if let Some(rest) = path.strip_prefix(unc) {
            return format!(r"\\{rest}");
        }
    }
    path.strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix("//?/"))
        .unwrap_or(path)
        .to_owned()
}

/// Whether two paths name the same folder, as [`folder_key`] compares them
/// for the system YIEN runs on.
///
/// ```
/// assert!(yien_proto::same_folder("C:/Code/yien/", "C:/Code//yien"));
/// assert!(!yien_proto::same_folder("C:/Code/yien", "C:/Code/other"));
/// ```
pub fn same_folder(a: &str, b: &str) -> bool {
    let windows = cfg!(windows);
    folder_key(a, windows) == folder_key(b, windows)
}

/// Whether `path` is the folder `root` or lies inside it, both already
/// resolved (links and `..` followed by the caller), compared as
/// [`folder_key`] compares them. A sibling whose name starts like the root
/// (`C:/Code/yien2` for `C:/Code/yien`) is outside.
///
/// ```
/// use yien_proto::path_within;
///
/// assert!(path_within(r"\\?\C:\Code\Yien", r"C:\code\yien\src\main.rs", true));
/// assert!(path_within("/home/e/proj", "/home/e/proj", false));
/// assert!(!path_within("/home/e/proj", "/home/e/proj2/x", false));
/// assert!(!path_within("/home/e/proj", "/home/e/Proj/x", false));
/// ```
pub fn path_within(root: &str, path: &str, windows: bool) -> bool {
    let root = folder_key(root, windows);
    let path = folder_key(path, windows);
    if root.is_empty() || path.is_empty() {
        return false;
    }
    if path == root {
        return true;
    }
    let Some(rest) = path.strip_prefix(&root) else {
        return false;
    };
    // A root key ends with `/` only when it is a drive or the filesystem root.
    root.ends_with('/') || rest.starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rnf_14_a_path_is_within_its_root_and_never_a_neighbour() {
        let root = r"C:\Code\yien";
        for inside in [
            r"C:\Code\yien",
            r"C:\Code\yien\",
            r"C:\Code\yien\src\main.rs",
            r"c:\code\YIEN\.claude\settings.json",
            r"\\?\C:\Code\yien\a b\c",
            "C:/Code//yien/x",
        ] {
            assert!(path_within(root, inside, true), "{inside}");
        }
        for outside in [
            r"C:\Code\yien2",
            r"C:\Code\yien2\x",
            r"C:\Code",
            r"C:\Code\other\yien",
            r"D:\Code\yien\x",
            r"\\server\share\Code\yien",
            "",
        ] {
            assert!(!path_within(root, outside, true), "{outside}");
        }
        assert!(
            !path_within("", r"C:\x", true),
            "an empty root holds nothing"
        );
    }

    #[test]
    fn rnf_14_drive_and_filesystem_roots_hold_everything_below_them() {
        assert!(path_within(r"C:\", r"C:\x\y", true));
        assert!(!path_within(r"C:\", r"D:\x", true));
        assert!(path_within("/", "/home/e", false));
        assert!(path_within(r"\\server\share", r"\\server\share\x", true));
        assert!(!path_within(r"\\server\share", r"\\server\shared\x", true));
    }

    #[test]
    fn rnf_14_unix_paths_keep_case_when_checking_containment() {
        assert!(path_within("/home/e/proj", "/home/e/proj/src", false));
        assert!(!path_within("/home/e/proj", "/home/e/PROJ/src", false));
        assert!(!path_within("/home/e/proj", "/home/e/proj-old", false));
    }

    #[test]
    fn rf_esp_01_spellings_of_one_windows_folder_share_a_key() {
        let key = folder_key("C:/Code/yien", true);
        for spelling in [
            r"C:\Code\yien",
            r"C:\Code\yien\",
            r"c:\code\YIEN",
            r"\\?\C:\Code\yien",
            "C:/Code//yien/",
            "  C:/Code/yien  ",
        ] {
            assert_eq!(folder_key(spelling, true), key, "{spelling}");
        }
        assert_ne!(folder_key("C:/Code/yien2", true), key);
        assert_ne!(folder_key("C:/Code", true), key);
    }

    #[test]
    fn rf_esp_01_unix_names_keep_case_backslashes_and_spaces() {
        assert_ne!(
            folder_key("/home/e/Proj", false),
            folder_key("/home/e/proj", false)
        );
        assert_eq!(folder_key("/home/e/proj/", false), "/home/e/proj");
        assert_eq!(folder_key(r"/home/e/a\b\", false), r"/home/e/a\b\");
        assert_eq!(folder_key("/home/e/sp ", false), "/home/e/sp ");
        assert_ne!(folder_key(" /home/e", false), folder_key("/home/e", false));
    }

    #[test]
    fn roots_and_shares_keep_their_separators() {
        assert_eq!(folder_key("/", false), "/");
        assert_eq!(folder_key(r"C:\", true), "c:/");
        assert_eq!(folder_key("C:/", true), "c:/");
        assert_eq!(folder_key(r"\\server\share\", true), "//server/share");
        assert_eq!(folder_key(r"\\server\\share", true), "//server/share");
    }

    #[test]
    fn a_verbatim_unc_path_is_the_share_it_names() {
        let share = folder_key(r"\\server\share\proj", true);
        assert_eq!(folder_key(r"\\?\UNC\server\share\proj", true), share);
        assert_eq!(folder_key("//?/UNC/server/share/proj/", true), share);
    }
}
