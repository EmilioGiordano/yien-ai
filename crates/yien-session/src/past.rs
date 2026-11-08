//! Reading an agent CLI's own session files (RF-SES-07): which folder holds a
//! project's sessions, and the summary of one session file. Pure: the caller
//! lists folders and opens files, and hands the bytes in through `Read`.
//!
//! Claude Code keeps each session in
//! `~/.claude/projects/<encoded project folder>/<session id>.jsonl`, one JSON
//! record per line. The files are the CLI's, never YIEN's: they are only read.

use std::io::{self, BufRead, BufReader, Read};

use serde::Deserialize;

/// The most bytes read from one session file. The title is near the start;
/// past this the file is not read, and its prompt count is unknown.
pub const SESSION_READ_BUDGET: u64 = 256 * 1024;

/// The longest record kept for parsing; a longer one (a pasted image, a large
/// tool result) is skipped without being held in memory.
pub const MAX_RECORD_BYTES: usize = 64 * 1024;

/// The most characters of a session's title, the ellipsis included.
pub const TITLE_CHARS: usize = 80;

/// What a session file says about itself, as far as it was read.
///
/// Its `Debug` leaves the title out: it is the user's prompt, which must
/// never reach a log.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SessionSummary {
    /// The first prompt the user typed, on one line, at most [`TITLE_CHARS`]
    /// characters; `None` when none was found within the read budget.
    pub title: Option<String>,
    /// The folder the session started in, from its first record that names
    /// one.
    pub cwd: Option<String>,
    /// How many prompts the user typed; `None` when the file was not read to
    /// its end.
    pub prompts: Option<u32>,
}

impl std::fmt::Debug for SessionSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSummary")
            .field(
                "title_chars",
                &self.title.as_ref().map(|t| t.chars().count()),
            )
            .field("cwd", &self.cwd)
            .field("prompts", &self.prompts)
            .finish()
    }
}

/// The name Claude Code gives the folder of a project's sessions: the
/// project folder with every character other than an ASCII letter or digit
/// turned into `-`, one per UTF-16 unit, as the CLI does. A `\\?\` prefix
/// and trailing separators are dropped first.
///
/// ```
/// use yien_session::claude_project_dir_name;
///
/// assert_eq!(claude_project_dir_name(r"C:\Users\me\My App.v2"), "C--Users-me-My-App-v2");
/// assert_eq!(claude_project_dir_name("C:/Code/yien/"), "C--Code-yien");
/// assert_eq!(claude_project_dir_name("/home/me/.config"), "-home-me--config");
/// ```
pub fn claude_project_dir_name(root: &str) -> String {
    let root = root
        .strip_prefix(r"\\?\")
        .or_else(|| root.strip_prefix("//?/"))
        .unwrap_or(root);
    let mut root = root;
    while root.len() > 1
        && (root.ends_with('/') || root.ends_with('\\'))
        && !root.ends_with(":\\")
        && !root.ends_with(":/")
    {
        root = &root[..root.len() - 1];
    }
    let mut name = String::with_capacity(root.len());
    for ch in root.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch);
        } else {
            for _ in 0..ch.len_utf16() {
                name.push('-');
            }
        }
    }
    name
}

/// Whether the session folder `name` belongs to the project at `root`.
/// With `windows`, letter case does not matter, as on that file system.
///
/// The encoding is lossy (`a b`, `a.b` and `a-b` share a folder), so a
/// session's own `cwd` settles it where the file names one.
///
/// ```
/// use yien_session::claude_dir_matches;
///
/// assert!(claude_dir_matches("c--code-yien", r"C:\Code\Yien", true));
/// assert!(!claude_dir_matches("c--code-yien", r"C:\Code\Yien", false));
/// ```
pub fn claude_dir_matches(name: &str, root: &str, windows: bool) -> bool {
    let wanted = claude_project_dir_name(root);
    if windows {
        name.eq_ignore_ascii_case(&wanted)
    } else {
        name == wanted
    }
}

/// Whether `id` has the shape of a session id: a UUID, 8-4-4-4-12 hex
/// digits. Anything else is refused before it reaches a command line.
///
/// ```
/// use yien_session::is_session_id;
///
/// assert!(is_session_id("4f1c2a3b-0000-4000-8000-0123456789ab"));
/// assert!(!is_session_id("--settings"));
/// ```
pub fn is_session_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(at, b)| match at {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Reads the summary of one Claude Code session file: its first prompt, its
/// folder and, when the whole file fits in `budget` bytes, its prompt count.
///
/// At most `budget + 1` bytes are read, and no record longer than
/// [`MAX_RECORD_BYTES`] is held. Lines that are not JSON, or not the shape
/// expected, are skipped: a file the CLI is still writing ends in half a
/// record.
///
/// ```
/// use yien_session::read_claude_session;
///
/// let file = concat!(
///     r#"{"type":"user","cwd":"C:\\Code\\yien","message":{"content":"fix the build"}}"#, "\n",
///     "not json\n",
/// );
/// let summary = read_claude_session(file.as_bytes(), 1024)?;
/// assert_eq!(summary.title.as_deref(), Some("fix the build"));
/// assert_eq!(summary.cwd.as_deref(), Some(r"C:\Code\yien"));
/// assert_eq!(summary.prompts, Some(1));
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn read_claude_session(reader: impl Read, budget: u64) -> io::Result<SessionSummary> {
    let mut lines = Lines {
        inner: BufReader::new(reader.take(budget.saturating_add(1))),
        read: 0,
    };
    let mut summary = SessionSummary::default();
    let mut prompts = 0u32;
    let mut line = Vec::new();
    while let Some(kept) = lines.next(&mut line)? {
        // A line that ends past the budget was cut by it.
        if !kept || lines.read > budget {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Record>(&line) else {
            continue;
        };
        if summary.cwd.is_none() {
            summary.cwd = record.cwd.clone().filter(|c| !c.is_empty());
        }
        let Some(prompt) = record.prompt() else {
            continue;
        };
        prompts = prompts.saturating_add(1);
        if summary.title.is_none() {
            summary.title = one_line(prompt);
        }
    }
    if lines.read <= budget {
        summary.prompts = Some(prompts);
    }
    Ok(summary)
}

/// Lines of at most [`MAX_RECORD_BYTES`]; longer ones are consumed and
/// reported as skipped without being stored.
struct Lines<R> {
    inner: R,
    /// Bytes consumed so far.
    read: u64,
}

impl<R: BufRead> Lines<R> {
    /// The next line into `buf`, without its newline: `Some(true)` when kept,
    /// `Some(false)` when it was too long, `None` at the end.
    fn next(&mut self, buf: &mut Vec<u8>) -> io::Result<Option<bool>> {
        buf.clear();
        let mut too_long = false;
        let mut any = false;
        loop {
            let available = match self.inner.fill_buf() {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if available.is_empty() {
                return Ok(any.then_some(!too_long));
            }
            any = true;
            let (chunk, done) = match available.iter().position(|b| *b == b'\n') {
                Some(at) => (&available[..at], at + 1),
                None => (available, available.len()),
            };
            if !too_long {
                if buf.len() + chunk.len() <= MAX_RECORD_BYTES {
                    buf.extend_from_slice(chunk);
                } else {
                    too_long = true;
                    buf.clear();
                }
            }
            let ended = done > chunk.len();
            self.inner.consume(done);
            self.read += done as u64;
            if ended {
                return Ok(Some(!too_long));
            }
        }
    }
}

/// The fields of a record this reader looks at; the rest are ignored.
#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(rename = "isMeta", default)]
    is_meta: bool,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: serde_json::Value,
}

/// What the CLI itself wraps in tags and records as a user turn: slash
/// commands, their output, background task notices. Not prompts.
const MACHINE_PREFIXES: &[&str] = &["<command-", "<local-command-", "<task-notification"];

impl Record {
    /// The text of a prompt the user typed, if this record is one. Tool
    /// results, meta records and subagent turns are not.
    fn prompt(&self) -> Option<&str> {
        if self.kind != "user" || self.is_meta || self.is_sidechain {
            return None;
        }
        let content = &self.message.as_ref()?.content;
        let text = match content {
            serde_json::Value::String(text) => text.as_str(),
            serde_json::Value::Array(blocks) => blocks.iter().find_map(|block| {
                (block.get("type")?.as_str()? == "text").then_some(())?;
                block.get("text")?.as_str()
            })?,
            _ => return None,
        };
        let trimmed = text.trim_start();
        if trimmed.is_empty() || MACHINE_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
            return None;
        }
        Some(text)
    }
}

/// `text` on one line: runs of whitespace and control characters become one
/// space, and past [`TITLE_CHARS`] it is cut with an ellipsis.
fn one_line(text: &str) -> Option<String> {
    let mut chars: Vec<char> = Vec::with_capacity(TITLE_CHARS + 1);
    let mut gap = false;
    for ch in text.chars() {
        if ch.is_whitespace() || ch.is_control() {
            gap = !chars.is_empty();
            continue;
        }
        if gap {
            chars.push(' ');
            gap = false;
        }
        chars.push(ch);
        if chars.len() > TITLE_CHARS {
            chars.truncate(TITLE_CHARS - 1);
            while chars.last() == Some(&' ') {
                chars.pop();
            }
            chars.push('…');
            break;
        }
    }
    (!chars.is_empty()).then(|| chars.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn user(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "cwd": "C:\\Code\\yien",
            "message": { "role": "user", "content": text },
        })
        .to_string()
    }

    fn file(lines: &[String]) -> String {
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }

    fn read(text: &str) -> SessionSummary {
        read_claude_session(text.as_bytes(), SESSION_READ_BUDGET).unwrap()
    }

    /// Counts the bytes taken from the reader it wraps.
    struct Counting<R> {
        inner: R,
        taken: Rc<Cell<u64>>,
    }

    impl<R: Read> Read for Counting<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.taken.set(self.taken.get() + n as u64);
            Ok(n)
        }
    }

    #[test]
    fn rf_ses_07_folder_names_follow_the_cli_encoding() {
        // Shapes taken from real folder names: the drive colon and every
        // separator become `-`, and so do dots, spaces and other symbols.
        for (root, name) in [
            (r"C:\Users\me", "C--Users-me"),
            (
                r"C:\Users\me\AppData\Roaming\.minecraft",
                "C--Users-me-AppData-Roaming--minecraft",
            ),
            (r"C:\Code\Portfolio - Kimi", "C--Code-Portfolio---Kimi"),
            (
                r"C:\Code\app\.claude\worktrees\x",
                "C--Code-app--claude-worktrees-x",
            ),
            (r"\\?\C:\Code\yien\", "C--Code-yien"),
            ("C:/Code/yien", "C--Code-yien"),
            (r"C:\", "C--"),
            ("/srv/app", "-srv-app"),
            ("C:\\C\u{f3}digo", "C--C-digo"),
            ("C:\\a\u{1f600}b", "C--a--b"),
        ] {
            assert_eq!(claude_project_dir_name(root), name, "{root}");
        }
    }

    #[test]
    fn rf_ses_07_folders_match_without_case_only_on_windows() {
        assert!(claude_dir_matches("C--Code-yien", r"C:\Code\yien", true));
        assert!(claude_dir_matches("c--code-YIEN", r"C:\Code\yien", true));
        assert!(!claude_dir_matches("c--code-YIEN", r"C:\Code\yien", false));
        assert!(!claude_dir_matches(
            "C--Code-yien-wt",
            r"C:\Code\yien",
            true
        ));
        assert!(!claude_dir_matches("C--Code", r"C:\Code\yien", true));
    }

    #[test]
    fn rf_ses_07_the_first_typed_prompt_is_the_title() {
        let raw = [
            r#"{"type":"bridge-session","sessionId":"x"}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"caveat"}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"user","message":{"content":"<local-command-stdout></local-command-stdout>"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"subagent task"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"image"},{"type":"text","text":"  what is\nthis?  "}]}}"#,
        ];
        let mut lines: Vec<String> = raw.iter().map(|l| (*l).to_owned()).collect();
        lines.push(user("second prompt"));
        lines.push(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"answer"}]}}"#
                .to_owned(),
        );
        let summary = read(&file(&lines));
        assert_eq!(summary.title.as_deref(), Some("what is this?"));
        assert_eq!(summary.prompts, Some(2));
        assert_eq!(summary.cwd.as_deref(), Some(r"C:\Code\yien"));
    }

    #[test]
    fn rf_ses_07_broken_lines_are_skipped() {
        let mut text = String::new();
        for line in [
            r#"{"type":"user","message":{"content":42}}"#,
            r#"{"type":"user","mess"#,
            "\u{0}garbage",
            "[1,2]",
            "\"str\"",
            "",
        ] {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str(&user("the real one"));
        text.push('\n');
        // A record the CLI is still writing.
        text.push_str(r#"{"type":"user","message":{"content":"half"#);
        let mut bytes = text.into_bytes();
        bytes.extend_from_slice(b"\n\xff\xfe not utf-8\n");
        let summary = read_claude_session(bytes.as_slice(), SESSION_READ_BUDGET).unwrap();
        assert_eq!(summary.title.as_deref(), Some("the real one"));
        assert_eq!(summary.prompts, Some(1));
    }

    #[test]
    fn rf_ses_07_titles_are_one_line_of_at_most_80_characters() {
        let long = "\u{f1}".repeat(200);
        let title = read(&file(&[user(&long)])).title.unwrap();
        assert_eq!(title.chars().count(), TITLE_CHARS);
        assert!(title.ends_with('\u{2026}'));

        let exact = "a".repeat(TITLE_CHARS);
        assert_eq!(read(&file(&[user(&exact)])).title.unwrap(), exact);

        let spaced = "word ".repeat(40);
        let title = read(&file(&[user(&spaced)])).title.unwrap();
        assert!(title.chars().count() <= TITLE_CHARS);
        assert!(title.ends_with("word\u{2026}"), "{title}");

        let lines = read(&file(&[user("first line\r\n\tsecond\u{7}  line")])).title;
        assert_eq!(lines.as_deref(), Some("first line second line"));
        assert_eq!(read(&file(&[user(" \n\t ")])).title, None);
    }

    #[test]
    fn rf_ses_07_a_huge_record_is_skipped_without_being_held() {
        let huge = user(&"x".repeat(MAX_RECORD_BYTES * 4));
        let text = file(&[huge, user("after the blob")]);
        let summary = read_claude_session(text.as_bytes(), u64::MAX - 1).unwrap();
        assert_eq!(summary.title.as_deref(), Some("after the blob"));
        assert_eq!(summary.prompts, Some(1));
    }

    #[test]
    fn rf_ses_07_a_huge_file_is_read_only_up_to_the_budget() {
        let mut text = file(&[user("near the start")]);
        let filler = r#"{"type":"assistant","message":{"content":"filler"}}"#;
        while text.len() < 4 * 1024 * 1024 {
            text.push_str(filler);
            text.push('\n');
        }
        text.push_str(&user("far past the budget"));
        text.push('\n');
        let taken = Rc::new(Cell::new(0));
        let reader = Counting {
            inner: text.as_bytes(),
            taken: Rc::clone(&taken),
        };
        let budget = 64 * 1024;
        let summary = read_claude_session(reader, budget).unwrap();
        assert!(taken.get() <= budget + 1, "read {} bytes", taken.get());
        assert_eq!(summary.title.as_deref(), Some("near the start"));
        assert_eq!(
            summary.prompts, None,
            "the count is unknown past the budget"
        );

        // A prompt that only appears past the budget is not found.
        let late = format!("{filler}\n{}\n", user("late"));
        let summary = read_claude_session(late.as_bytes(), 10).unwrap();
        assert_eq!(summary.title, None);
        assert_eq!(summary.prompts, None);
    }

    #[test]
    fn rf_ses_07_a_file_that_fits_the_budget_exactly_is_complete() {
        let text = file(&[user("fits")]);
        let len = text.len() as u64;
        let summary = read_claude_session(text.as_bytes(), len).unwrap();
        assert_eq!(summary.prompts, Some(1));
        let summary = read_claude_session(text.as_bytes(), len - 1).unwrap();
        assert_eq!(summary.prompts, None);
        assert_eq!(summary.title, None, "its only line was cut by the budget");
    }

    #[test]
    fn rf_ses_07_only_uuid_shaped_ids_are_session_ids() {
        assert!(is_session_id("939134b6-cc51-4e8a-ac5b-92248723c93a"));
        assert!(is_session_id("939134B6-CC51-4E8A-AC5B-92248723C93A"));
        for bad in [
            "",
            "memory",
            "939134b6cc514e8aac5b92248723c93a",
            "939134b6-cc51-4e8a-ac5b-92248723c93",
            "939134b6-cc51-4e8a-ac5b-92248723c93a0",
            "939134b6-cc51-4e8a-ac5b-92248723c93g",
            "--resume-cc51-4e8a-ac5b-92248723c93a",
            "939134b6 cc51-4e8a-ac5b-92248723c93a",
        ] {
            assert!(!is_session_id(bad), "{bad}");
        }
    }

    #[test]
    fn rf_ses_07_a_summary_never_shows_its_title_in_debug() {
        let summary = read(&file(&[user("secret plan")]));
        let shown = format!("{summary:?}");
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("title_chars: Some(11)"), "{shown}");
    }
}
