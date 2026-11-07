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

#[cfg(test)]
mod tests {
    use super::*;

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
