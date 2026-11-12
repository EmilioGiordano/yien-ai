//! A folder setting as written, turned into a path: `ui.tabs.default_dir`.

/// Expands a leading `~` (alone, or before `/` or `\`) to `home` and each
/// `%NAME%` to what `var` gives for `NAME`. What cannot be expanded (no
/// home, an unknown variable, a lone `%`) stays as written, so the folder
/// is then not found and the caller says so.
///
/// ```
/// let var = |name: &str| (name == "CODE").then(|| "D:\\Code".to_owned());
/// let home = Some("C:\\Users\\me");
/// assert_eq!(yien_config::expand_dir("~\\work", home, &var), "C:\\Users\\me\\work");
/// assert_eq!(yien_config::expand_dir("%CODE%\\yien", home, &var), "D:\\Code\\yien");
/// assert_eq!(yien_config::expand_dir("%NOPE%", home, &var), "%NOPE%");
/// ```
pub fn expand_dir(raw: &str, home: Option<&str>, var: &dyn Fn(&str) -> Option<String>) -> String {
    let (start, rest) = match (raw.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            (home.to_owned(), rest)
        }
        _ => (String::new(), raw),
    };
    let mut out = start;
    let mut rest = rest;
    while let Some(open) = rest.find('%') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after
            .find('%')
            .filter(|&close| close > 0)
            .and_then(|close| var(&after[..close]).map(|value| (value, close)));
        match value {
            Some((value, close)) => {
                out.push_str(&value);
                rest = &after[close + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(name: &str) -> Option<String> {
        match name {
            "USERPROFILE" => Some("C:\\Users\\me".to_owned()),
            "EMPTY" => Some(String::new()),
            "PCT" => Some("%USERPROFILE%".to_owned()),
            _ => None,
        }
    }

    fn expand(raw: &str) -> String {
        expand_dir(raw, Some("/home/me"), &vars)
    }

    #[test]
    fn rf_esp_03_a_leading_tilde_is_the_home_folder() {
        assert_eq!(expand("~"), "/home/me");
        assert_eq!(expand("~/code"), "/home/me/code");
        assert_eq!(expand("~\\code"), "/home/me\\code");
        assert_eq!(expand("~user/code"), "~user/code", "not another user's");
        assert_eq!(expand("C:/a~/b"), "C:/a~/b", "only at the start");
        assert_eq!(expand_dir("~/code", None, &vars), "~/code", "no home");
    }

    #[test]
    fn rf_esp_03_environment_variables_are_expanded_once() {
        assert_eq!(expand("%USERPROFILE%\\code"), "C:\\Users\\me\\code");
        assert_eq!(expand("%EMPTY%x"), "x");
        assert_eq!(
            expand("%PCT%"),
            "%USERPROFILE%",
            "a value is not expanded again"
        );
        assert_eq!(
            expand("~/%USERPROFILE%"),
            "/home/me/C:\\Users\\me",
            "both, in one path"
        );
    }

    #[test]
    fn rf_esp_03_what_cannot_be_expanded_stays_as_written() {
        assert_eq!(expand("%NOPE%\\a"), "%NOPE%\\a");
        assert_eq!(expand("50%"), "50%");
        assert_eq!(expand("%%"), "%%");
        assert_eq!(expand("a%NOPE%USERPROFILE%"), "a%NOPEC:\\Users\\me");
        assert_eq!(expand("D:/Code"), "D:/Code");
        assert_eq!(expand(""), "");
    }
}
