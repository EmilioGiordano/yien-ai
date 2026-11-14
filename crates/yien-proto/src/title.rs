//! What an agent CLI's terminal title says about its session (RF-PAN-12).

/// The longest session title a pane shows, in characters, `…` included.
///
/// ```
/// assert!(yien_proto::SESSION_TITLE_MAX >= 20);
/// ```
pub const SESSION_TITLE_MAX: usize = 60;

/// Titles an agent CLI sets that name no session: its own names.
const GENERIC: &[&str] = &["claude", "claude code", "codex", "openai codex", "opencode"];

/// OpenCode writes `OC | <session title>`.
const PREFIXES: &[&str] = &["OC |"];

/// The session title in `raw`, a terminal title an agent CLI set (OSC 0 or
/// 2), or `None` when it names no session.
///
/// Leading status glyphs (`✳`, `◐`, a braille spinner, anything before the
/// first letter or digit) and OpenCode's `OC | ` go. What is left names no
/// session when it is empty, starts like an absolute path, ends in `.exe`,
/// or equals (ignoring case) one of the CLIs' own names, one of `names`, or
/// a folder name in one of `folders`. Runs of whitespace and control
/// characters become one space; a longer title is cut to
/// [`SESSION_TITLE_MAX`] characters ending in `…`.
///
/// ```
/// use yien_proto::session_title;
///
/// let folders = ["C:/Code/yien"];
/// assert_eq!(session_title("✳ Fix login bug", &["claude"], &folders).as_deref(), Some("Fix login bug"));
/// assert_eq!(session_title("✳ Claude Code", &["claude"], &folders), None);
/// assert_eq!(session_title("⠹ yien", &["codex"], &folders), None);
/// ```
pub fn session_title(raw: &str, names: &[&str], folders: &[&str]) -> Option<String> {
    let text = raw.trim();
    if is_path(text) {
        return None;
    }
    let mut text = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    for prefix in PREFIXES {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim_start_matches(|c: char| !c.is_alphanumeric());
        }
    }
    let text = one_line(text);
    if text.is_empty() || is_path(&text) {
        return None;
    }
    let lower = text.to_lowercase();
    let same = |name: &str| !name.is_empty() && name.to_lowercase() == lower;
    let mut folder_names = folders
        .iter()
        .flat_map(|f| f.split(['/', '\\']))
        .filter(|n| !n.is_empty());
    if GENERIC.iter().any(|n| *n == lower)
        || names.iter().any(|n| same(n))
        || folder_names.any(same)
    {
        return None;
    }
    Some(cap(text))
}

/// Whether `text` starts like an absolute path (`C:\`, `C:/`, `\\`, `/`,
/// `~/`) or names an executable: Claude Code first sets its own file.
fn is_path(text: &str) -> bool {
    let mut chars = text.chars();
    let drive = matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(d), Some(':'), Some('\\' | '/')) if d.is_ascii_alphabetic()
    );
    let lower = text.to_ascii_lowercase();
    drive
        || text.starts_with(['/', '\\'])
        || text.starts_with("~/")
        || text.starts_with("~\\")
        || lower.ends_with(".exe")
}

/// `text` with each run of whitespace and control characters as one space,
/// and none at the ends.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
    {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

fn cap(text: String) -> String {
    if text.chars().count() <= SESSION_TITLE_MAX {
        return text;
    }
    let kept: String = text.chars().take(SESSION_TITLE_MAX - 1).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::{SESSION_TITLE_MAX, session_title};

    const FOLDERS: [&str; 2] = [
        r"C:\Users\me\Code\yien-ai\",
        "C:/Users/me/Code/yien-ai/crates",
    ];

    fn title(raw: &str) -> Option<String> {
        session_title(raw, &["claude", "Claude Code"], &FOLDERS)
    }

    #[test]
    fn rf_pan_12_status_glyphs_before_the_topic_go() {
        // Claude Code's ✳, ◐ and ◑ (2.1.323), Codex's braille spinner.
        for raw in [
            "✳ Fix login bug",
            "◐ Fix login bug",
            "◑ Fix login bug",
            "⠹ Fix login bug",
            "> Fix login bug",
            "* Fix login bug",
            "  ✶✻ Fix login bug  ",
        ] {
            assert_eq!(title(raw).as_deref(), Some("Fix login bug"), "{raw}");
        }
        // The topic keeps its own non-ASCII letters and digits.
        assert_eq!(title("✳ 修复登录").as_deref(), Some("修复登录"));
        assert_eq!(title("◐ 2fa flow").as_deref(), Some("2fa flow"));
    }

    #[test]
    fn rf_pan_12_opencode_names_its_session_after_a_prefix() {
        assert_eq!(
            title("OC | Fix login bug").as_deref(),
            Some("Fix login bug")
        );
        assert_eq!(session_title("OpenCode", &["opencode"], &FOLDERS), None);
        assert_eq!(session_title("OC | ", &["opencode"], &FOLDERS), None);
    }

    #[test]
    fn rf_pan_12_a_cli_naming_itself_is_no_session_title() {
        // What Claude Code 2.1.322 and 2.1.323 set before any topic.
        for raw in [
            r"C:\Users\me\.local\share\claude\versions\2.1.322",
            "claude",
            "✳ Claude Code",
            "◐ CLAUDE CODE",
            "codex",
            "OpenAI Codex",
            "opencode",
            "",
            "   ",
            "✳",
            "◐ ◑",
        ] {
            assert_eq!(title(raw), None, "{raw:?}");
        }
        // The pane's own names: a custom profile's id, its command.
        assert_eq!(
            session_title("✳ My Agent", &["my-agent", "My Agent"], &[]),
            None
        );
    }

    #[test]
    fn rf_pan_12_a_path_or_an_executable_is_no_session_title() {
        for raw in [
            r"C:\Program Files\tool\agent.exe",
            "C:/tools/agent",
            r"\\server\share\agent",
            "/usr/local/bin/claude",
            "~/bin/claude",
            r"~\bin\claude",
            "agent.EXE",
            "✳ D:\\work\\agent",
        ] {
            assert_eq!(title(raw), None, "{raw}");
        }
        // A slash inside a topic is not a path.
        assert_eq!(title("✳ Fix a/b test").as_deref(), Some("Fix a/b test"));
    }

    #[test]
    fn rf_pan_12_a_folder_name_is_no_session_title() {
        // Codex 0.167 sets its spinner and the project's folder name.
        for raw in ["⠹ yien-ai", "yien-ai", "YIEN-AI", "crates", "⠋ Code"] {
            assert_eq!(title(raw), None, "{raw}");
        }
        assert_eq!(title("yien-ai review").as_deref(), Some("yien-ai review"));
    }

    #[test]
    fn rf_pan_12_a_title_is_one_line_and_capped() {
        assert_eq!(
            title("✳ Fix\tthe\r\n  login\u{7} bug").as_deref(),
            Some("Fix the login bug")
        );
        let long = format!("✳ {}", "word ".repeat(40));
        let capped = title(&long).unwrap();
        assert_eq!(capped.chars().count(), SESSION_TITLE_MAX);
        assert!(capped.ends_with("word…"), "{capped}");
        let wide = format!("✳ {}", "界".repeat(100));
        let capped = title(&wide).unwrap();
        assert_eq!(capped.chars().count(), SESSION_TITLE_MAX);
        assert!(capped.ends_with('…'));
        let exact = "x".repeat(SESSION_TITLE_MAX);
        assert_eq!(title(&exact).as_deref(), Some(exact.as_str()));
    }

    #[test]
    fn rf_pan_12_the_topic_does_not_change_with_the_glyph() {
        let frames: Vec<_> = ["◐", "◑", "✳", "⠋", "⠙"]
            .iter()
            .map(|g| title(&format!("{g} Fix login bug")))
            .collect();
        assert!(frames.windows(2).all(|w| w[0] == w[1]));
    }
}
