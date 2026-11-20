//! Visible slices of a previewed file (RF-CTX-02, ADR-06): the lines shown in
//! the file pane's viewport, their syntax highlight spans and any notice when
//! the file is loading, binary, truncated or unreadable.
//!
//! ```
//! use yien_proto::file_preview::{FileFrame, FileLine, SyntaxKind, SyntaxSpan};
//!
//! let line = FileLine {
//!     number: 1,
//!     text: "fn main() {}".into(),
//!     spans: vec![SyntaxSpan { start: 0, end: 2, kind: SyntaxKind::Keyword }],
//! };
//! let frame = FileFrame {
//!     path: "src/main.rs".into(),
//!     language: Some("Rust".into()),
//!     top_line: 0,
//!     total_lines: 1,
//!     lines: vec![line],
//!     notice: None,
//! };
//! assert_eq!(frame.lines.len(), 1);
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

/// Semantic token category for syntax highlighting, resolved to the active
/// Sumi or Washi palette in the UI (never red, RN-01).
///
/// ```
/// use yien_proto::file_preview::SyntaxKind;
///
/// assert_eq!(SyntaxKind::default(), SyntaxKind::Plain);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntaxKind {
    /// Unstyled source text.
    #[default]
    Plain,
    /// A language keyword (`fn`, `let`, `pub`, `match`).
    Keyword,
    /// A type, trait, struct or module name.
    Type,
    /// A function or method name.
    Function,
    /// A string or character literal.
    String,
    /// A numeric or boolean literal.
    Number,
    /// A line or block comment.
    Comment,
    /// A macro, attribute or preprocessor directive.
    Macro,
}

/// One styled byte range within a [`FileLine`]'s text.
///
/// ```
/// use yien_proto::file_preview::{SyntaxKind, SyntaxSpan};
///
/// let span = SyntaxSpan { start: 0, end: 3, kind: SyntaxKind::Keyword };
/// assert_eq!(span.end - span.start, 3);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxSpan {
    /// Start byte offset in [`FileLine::text`], on a UTF-8 character boundary.
    pub start: usize,
    /// End byte offset in [`FileLine::text`], on a UTF-8 character boundary.
    pub end: usize,
    /// Which syntax token colour applies.
    pub kind: SyntaxKind,
}

/// One visible line of a previewed file. Its `Debug` hides the source text so
/// file contents never reach a log (plan §5.4).
///
/// ```
/// use yien_proto::file_preview::FileLine;
///
/// let line = FileLine {
///     number: 12,
///     text: "let secret = 42;".into(),
///     spans: Vec::new(),
/// };
/// assert!(!format!("{line:?}").contains("secret"));
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLine {
    /// 1-based line number in the file.
    pub number: usize,
    /// The line's text, with tabs expanded and without trailing line breaks.
    pub text: String,
    /// Syntax spans within `text`, sorted by `start`.
    pub spans: Vec<SyntaxSpan>,
}

impl fmt::Debug for FileLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileLine")
            .field("number", &self.number)
            .field("text_bytes", &self.text.len())
            .field("spans", &self.spans.len())
            .finish()
    }
}

/// Why a previewed file is not shown in full, or cannot be shown as text
/// (RF-CTX-02).
///
/// ```
/// use yien_proto::file_preview::FileNotice;
///
/// let binary = FileNotice::Binary { bytes: 4096 };
/// assert!(matches!(binary, FileNotice::Binary { .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileNotice {
    /// The file is being read and highlighted off the UI thread.
    Loading,
    /// The file is binary; never rendered as garbage text.
    Binary {
        /// File size in bytes.
        bytes: u64,
    },
    /// The file exceeded the preview byte or line cap; the capped prefix is
    /// shown with this notice.
    Truncated {
        /// Bytes of the prefix shown.
        bytes_shown: usize,
        /// Total file size in bytes on disk.
        total_bytes: u64,
        /// Lines shown in the preview.
        lines_shown: usize,
        /// Whether the line cap was reached before the byte cap.
        capped_by_lines: bool,
    },
    /// The file could not be read.
    Error(String),
}

/// The visible viewport slice of a previewed file (ADR-06: the UI holds only
/// the visible rows, never the full document). Its `Debug` omits line text.
///
/// ```
/// use yien_proto::file_preview::{FileFrame, FileLine};
///
/// let frame = FileFrame {
///     path: "C:/work/src/lib.rs".into(),
///     language: Some("Rust".into()),
///     top_line: 0,
///     total_lines: 1,
///     lines: vec![FileLine { number: 1, text: "secret".into(), spans: vec![] }],
///     notice: None,
/// };
/// assert!(!format!("{frame:?}").contains("secret"));
/// ```
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FileFrame {
    /// The path of the file shown.
    pub path: String,
    /// Detected language label, if any.
    pub language: Option<String>,
    /// 0-based index of the first line in `lines`.
    pub top_line: usize,
    /// Total number of lines in the loaded (or capped) file.
    pub total_lines: usize,
    /// The visible rows starting at `top_line`.
    pub lines: Vec<FileLine>,
    /// Notice when loading, binary, truncated or unreadable.
    pub notice: Option<FileNotice>,
}

impl fmt::Debug for FileFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileFrame")
            .field("path", &self.path)
            .field("language", &self.language)
            .field("top_line", &self.top_line)
            .field("total_lines", &self.total_lines)
            .field("lines", &self.lines.len())
            .field("notice", &self.notice)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_ctx_02_file_frame_round_trips_and_hides_source_in_debug() {
        let frame = FileFrame {
            path: "C:/Code/yien/src/main.rs".into(),
            language: Some("Rust".into()),
            top_line: 10,
            total_lines: 250,
            lines: vec![FileLine {
                number: 11,
                text: "let private_token = \"xyz\";".into(),
                spans: vec![
                    SyntaxSpan {
                        start: 0,
                        end: 3,
                        kind: SyntaxKind::Keyword,
                    },
                    SyntaxSpan {
                        start: 20,
                        end: 25,
                        kind: SyntaxKind::String,
                    },
                ],
            }],
            notice: Some(FileNotice::Truncated {
                bytes_shown: 2_097_152,
                total_bytes: 5_000_000,
                lines_shown: 20_000,
                capped_by_lines: true,
            }),
        };
        let json = serde_json::to_string(&frame).unwrap();
        let back: FileFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, frame);
        let debug = format!("{frame:?}");
        assert!(!debug.contains("private_token"), "{debug}");
        assert!(!debug.contains("xyz"), "{debug}");
    }
}
