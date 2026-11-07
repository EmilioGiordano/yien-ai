//! Folder paths as keys, so the UI and the core agree on when two spellings
//! name the same project folder (RF-ESP-01).

/// A folder path in a form fit for comparison: without a `\\?\` prefix, with
/// `/` as the only separator, no repeated or trailing separator (except a
/// root's), and lower case when `case_insensitive`. It does not touch the
/// disk, so links and `..` are compared as written.
///
/// ```
/// use yien_proto::folder_key;
///
/// assert_eq!(folder_key(r"\\?\C:\Code\Yien\", true), "c:/code/yien");
/// assert_eq!(folder_key("/home/e//proj/", false), "/home/e/proj");
/// assert_eq!(folder_key(r"C:\", true), "c:/");
/// ```
pub fn folder_key(path: &str, case_insensitive: bool) -> String {
    let path = path.trim();
    let path = path
        .strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix("//?/"))
        .unwrap_or(path);
    let mut key = String::with_capacity(path.len());
    for ch in path.chars() {
        let ch = if ch == '\\' { '/' } else { ch };
        // A leading `//` is a UNC share and keeps both slashes.
        if ch == '/' && key.ends_with('/') && key.len() > 1 {
            continue;
        }
        key.push(ch);
    }
    while key.len() > 1 && key.ends_with('/') && !key.ends_with(":/") && key != "//" {
        key.pop();
    }
    if case_insensitive {
        key = key.to_lowercase();
    }
    key
}

/// Whether two paths name the same folder, as [`folder_key`] compares them:
/// ignoring case on Windows only.
///
/// ```
/// assert!(yien_proto::same_folder("C:/Code/yien/", r"C:\Code\yien"));
/// assert!(!yien_proto::same_folder("C:/Code/yien", "C:/Code/other"));
/// ```
pub fn same_folder(a: &str, b: &str) -> bool {
    let fold = cfg!(windows);
    folder_key(a, fold) == folder_key(b, fold)
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
    fn rf_esp_01_case_counts_where_the_file_system_minds_it() {
        assert_ne!(
            folder_key("/home/e/Proj", false),
            folder_key("/home/e/proj", false)
        );
        assert_eq!(folder_key("/home/e/proj/", false), "/home/e/proj");
    }

    #[test]
    fn roots_and_shares_keep_their_separators() {
        assert_eq!(folder_key("/", false), "/");
        assert_eq!(folder_key(r"C:\", true), "c:/");
        assert_eq!(folder_key("C:/", true), "c:/");
        assert_eq!(folder_key(r"\\server\share\", true), "//server/share");
        assert_eq!(folder_key(r"\\server\\share", true), "//server/share");
    }
}
