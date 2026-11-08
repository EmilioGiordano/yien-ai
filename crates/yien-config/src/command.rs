//! The `editor` setting as a program and its arguments.

/// Splits a command line on whitespace, keeping double-quoted parts whole
/// so a program path with spaces can be quoted. `None` when there is no
/// program or a quote is left open.
///
/// ```
/// let parts = yien_config::split_command(r#""C:\Program Files\Ed\ed.exe" --new"#).unwrap();
/// assert_eq!(parts, [r"C:\Program Files\Ed\ed.exe", "--new"]);
/// ```
pub fn split_command(line: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return None;
    }
    if started {
        parts.push(current);
    }
    (!parts.is_empty() && !parts[0].is_empty()).then_some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_split_on_whitespace() {
        assert_eq!(
            split_command("  code  --reuse-window ").unwrap(),
            ["code", "--reuse-window"]
        );
        assert_eq!(split_command("notepad").unwrap(), ["notepad"]);
    }

    #[test]
    fn quotes_keep_spaces_and_can_be_empty() {
        assert_eq!(
            split_command(r#"ed "a b" "" x"#).unwrap(),
            ["ed", "a b", "", "x"]
        );
        assert_eq!(
            split_command(r#"ed --opt="a b""#).unwrap(),
            ["ed", "--opt=a b"]
        );
    }

    #[test]
    fn no_program_or_an_open_quote_is_none() {
        assert_eq!(split_command(""), None);
        assert_eq!(split_command("   "), None);
        assert_eq!(split_command("\"\" x"), None);
        assert_eq!(split_command("\"ed"), None);
    }
}
