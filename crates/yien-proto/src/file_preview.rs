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
//!     rendered: None,
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
///     rendered: None,
/// };
/// assert!(!format!("{frame:?}").contains("secret"));
/// ```
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FileFrame {
    /// The path of the file shown.
    pub path: String,
    /// Detected language label, if any.
    pub language: Option<String>,
    /// 0-based index of the first line in `lines`, or of the first row in
    /// `rendered` when that is set.
    pub top_line: usize,
    /// Total number of lines in the loaded (or capped) file, or of rendered
    /// rows when `rendered` is set.
    pub total_lines: usize,
    /// The visible rows starting at `top_line`; empty when `rendered` is set.
    pub lines: Vec<FileLine>,
    /// Notice when loading, binary, truncated or unreadable.
    pub notice: Option<FileNotice>,
    /// The visible rendered Markdown rows starting at `top_line`, when the
    /// pane shows a Markdown file in [`PreviewMode::Rendered`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<Vec<MdRow>>,
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
            .field("rendered", &self.rendered.as_ref().map(Vec::len))
            .finish()
    }
}

/// The file extensions shown as Markdown, lowercase (RF-CTX-02).
///
/// ```
/// assert!(yien_proto::file_preview::MARKDOWN_EXTENSIONS.contains(&"md"));
/// ```
pub const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown"];

/// Whether a previewed file is Markdown by its extension, in any case.
///
/// ```
/// use yien_proto::file_preview::is_markdown_path;
///
/// assert!(is_markdown_path("docs/README.md"));
/// assert!(is_markdown_path("C:\\work\\NOTES.Markdown"));
/// assert!(!is_markdown_path("src/main.rs"));
/// assert!(!is_markdown_path("md"));
/// ```
pub fn is_markdown_path(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && MARKDOWN_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
    })
}

/// How a file preview pane shows a Markdown file (RF-CTX-02): rendered, the
/// default, or its highlighted source. Other files always show their source.
///
/// ```
/// use yien_proto::file_preview::PreviewMode;
///
/// assert_eq!(PreviewMode::default(), PreviewMode::Rendered);
/// assert_eq!(PreviewMode::Rendered.toggled(), PreviewMode::Source);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMode {
    /// Headings, lists, tables and code blocks drawn as a document.
    #[default]
    Rendered,
    /// The file's text, line by line, with syntax highlighting.
    Source,
}

impl PreviewMode {
    /// The other mode.
    pub fn toggled(self) -> Self {
        match self {
            Self::Rendered => Self::Source,
            Self::Source => Self::Rendered,
        }
    }

    /// Its name in test-control state: `"rendered"` or `"source"`.
    ///
    /// ```
    /// assert_eq!(yien_proto::file_preview::PreviewMode::Source.key(), "source");
    /// ```
    pub fn key(self) -> &'static str {
        match self {
            Self::Rendered => "rendered",
            Self::Source => "source",
        }
    }
}

/// The inline styles of a run of rendered Markdown text.
///
/// ```
/// use yien_proto::file_preview::MdStyle;
///
/// assert!(MdStyle::default().is_plain());
/// assert!(!MdStyle { strong: true, ..MdStyle::default() }.is_plain());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct MdStyle {
    /// `**bold**`.
    #[serde(default)]
    pub strong: bool,
    /// `*italic*`.
    #[serde(default)]
    pub emphasis: bool,
    /// `~~struck~~`.
    #[serde(default)]
    pub strike: bool,
    /// `` `inline code` ``.
    #[serde(default)]
    pub code: bool,
    /// The text of a link.
    #[serde(default)]
    pub link: bool,
    /// The placeholder of an image, which is never loaded.
    #[serde(default)]
    pub image: bool,
}

impl MdStyle {
    /// No style at all.
    pub fn is_plain(self) -> bool {
        self == Self::default()
    }
}

/// One styled byte range of an [`MdText`].
///
/// ```
/// use yien_proto::file_preview::{MdSpan, MdStyle};
///
/// let code = MdStyle { code: true, ..MdStyle::default() };
/// let span = MdSpan { start: 0, end: 4, style: code };
/// assert_eq!(span.end - span.start, 4);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdSpan {
    /// Start byte offset in [`MdText::text`], on a UTF-8 character boundary.
    pub start: usize,
    /// End byte offset in [`MdText::text`], on a UTF-8 character boundary.
    pub end: usize,
    /// The styles that apply.
    pub style: MdStyle,
}

/// Rendered inline Markdown: its text, markup removed, and the styled ranges
/// in it, sorted and never overlapping. Its `Debug` hides the text.
///
/// ```
/// use yien_proto::file_preview::MdText;
///
/// let text = MdText { text: "private".into(), spans: Vec::new() };
/// assert!(!format!("{text:?}").contains("private"));
/// ```
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MdText {
    /// The text as shown; a hard line break is a line feed.
    pub text: String,
    /// Styled ranges of `text`; text outside them is plain.
    #[serde(default)]
    pub spans: Vec<MdSpan>,
}

impl fmt::Debug for MdText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MdText")
            .field("text_bytes", &self.text.len())
            .field("spans", &self.spans.len())
            .finish()
    }
}

/// What marks a list item.
///
/// ```
/// use yien_proto::file_preview::MdMarker;
///
/// assert_ne!(MdMarker::Task { done: true }, MdMarker::Bullet);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MdMarker {
    /// An unordered item.
    Bullet,
    /// An ordered item with its number.
    Number(u64),
    /// A task list item, checked or not.
    Task {
        /// `- [x]`.
        done: bool,
    },
}

/// How a table column aligns its cells.
///
/// ```
/// assert_eq!(yien_proto::file_preview::MdAlign::default(), yien_proto::file_preview::MdAlign::None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MdAlign {
    /// No alignment given: start.
    #[default]
    None,
    /// `:---`.
    Left,
    /// `:---:`.
    Center,
    /// `---:`.
    Right,
}

/// One table column: its alignment and the characters its widest cell needs,
/// so rows sliced apart still line up.
///
/// ```
/// use yien_proto::file_preview::{MdAlign, MdColumn};
///
/// let column = MdColumn { width: 12, align: MdAlign::Right };
/// assert_eq!(column.width, 12);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdColumn {
    /// The widest cell of the column in characters, at least 1 and capped.
    pub width: u16,
    /// The column's alignment.
    pub align: MdAlign,
}

/// What a rendered Markdown row draws.
///
/// ```
/// use yien_proto::file_preview::{MdBlock, MdText};
///
/// let heading = MdBlock::Heading { level: 1, text: MdText::default() };
/// assert!(matches!(heading, MdBlock::Heading { level: 1, .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MdBlock {
    /// A heading, `level` 1 to 6.
    Heading {
        /// `#` is 1, `######` is 6.
        level: u8,
        /// Its text.
        text: MdText,
    },
    /// A paragraph, or part of a long one.
    Paragraph {
        /// Its text.
        text: MdText,
    },
    /// The first paragraph of a list item, after its marker.
    Item {
        /// The bullet, number or checkbox.
        marker: MdMarker,
        /// Its text.
        text: MdText,
    },
    /// One line of a code block, highlighted when its language is known.
    Code {
        /// The line; `number` counts from 1 within the block.
        line: FileLine,
    },
    /// One row of a table, padded to the table's columns.
    TableRow {
        /// One text per column.
        cells: Vec<MdText>,
        /// The table's columns.
        columns: Vec<MdColumn>,
        /// It is the header row.
        header: bool,
    },
    /// A horizontal rule.
    Rule,
}

/// One row of a rendered Markdown file: the unit the preview scrolls by.
/// Long paragraphs and code blocks span several rows.
///
/// ```
/// use yien_proto::file_preview::{MdBlock, MdRow};
///
/// let rule = MdRow { block: MdBlock::Rule, quote: 0, indent: 0, first: true, last: true };
/// assert!(rule.first && rule.last);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdRow {
    /// What it draws.
    pub block: MdBlock,
    /// How many block quotes hold it.
    #[serde(default)]
    pub quote: u8,
    /// How many lists hold it; an item's marker hangs in the last level.
    #[serde(default)]
    pub indent: u8,
    /// It starts its block: a code block's or table's top edge.
    #[serde(default)]
    pub first: bool,
    /// It ends its block, so space follows it.
    #[serde(default)]
    pub last: bool,
}

impl MdRow {
    /// The row's text as a reader sees it, markup removed: for test control.
    ///
    /// ```
    /// use yien_proto::file_preview::{MdBlock, MdMarker, MdRow, MdText};
    ///
    /// let item = MdRow {
    ///     block: MdBlock::Item {
    ///         marker: MdMarker::Number(3),
    ///         text: MdText { text: "Run it".into(), spans: Vec::new() },
    ///     },
    ///     quote: 0,
    ///     indent: 1,
    ///     first: true,
    ///     last: false,
    /// };
    /// assert_eq!(item.plain_text(), "3. Run it");
    /// ```
    pub fn plain_text(&self) -> String {
        match &self.block {
            MdBlock::Heading { text, .. } | MdBlock::Paragraph { text } => text.text.clone(),
            MdBlock::Item { marker, text } => format!("{} {}", marker_label(*marker), text.text),
            MdBlock::Code { line } => line.text.clone(),
            MdBlock::TableRow { cells, .. } => cells
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join(" \u{2502} "),
            MdBlock::Rule => "\u{2500}\u{2500}\u{2500}".to_owned(),
        }
    }
}

/// The text a list marker is drawn as: a bullet, `3.`, `[x]` or `[ ]`.
///
/// ```
/// use yien_proto::file_preview::{MdMarker, marker_label};
///
/// assert_eq!(marker_label(MdMarker::Number(12)), "12.");
/// assert_eq!(marker_label(MdMarker::Task { done: false }), "[ ]");
/// ```
pub fn marker_label(marker: MdMarker) -> String {
    match marker {
        MdMarker::Bullet => "\u{2022}".to_owned(),
        MdMarker::Number(n) => format!("{n}."),
        MdMarker::Task { done: true } => "[x]".to_owned(),
        MdMarker::Task { done: false } => "[ ]".to_owned(),
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
            rendered: None,
        };
        let json = serde_json::to_string(&frame).unwrap();
        let back: FileFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, frame);
        let debug = format!("{frame:?}");
        assert!(!debug.contains("private_token"), "{debug}");
        assert!(!debug.contains("xyz"), "{debug}");
    }

    #[test]
    fn rf_ctx_02_rendered_markdown_frame_round_trips_and_hides_text_in_debug() {
        let text = |s: &str| MdText {
            text: s.into(),
            spans: vec![MdSpan {
                start: 0,
                end: 3,
                style: MdStyle {
                    strong: true,
                    ..MdStyle::default()
                },
            }],
        };
        let row = |block| MdRow {
            block,
            quote: 1,
            indent: 2,
            first: true,
            last: false,
        };
        let rows = vec![
            row(MdBlock::Heading {
                level: 2,
                text: text("secret heading"),
            }),
            row(MdBlock::Item {
                marker: MdMarker::Task { done: true },
                text: text("secret item"),
            }),
            row(MdBlock::Code {
                line: FileLine {
                    number: 1,
                    text: "secret code".into(),
                    spans: Vec::new(),
                },
            }),
            row(MdBlock::TableRow {
                cells: vec![text("secret cell"), MdText::default()],
                columns: vec![
                    MdColumn {
                        width: 11,
                        align: MdAlign::Right,
                    },
                    MdColumn {
                        width: 1,
                        align: MdAlign::None,
                    },
                ],
                header: true,
            }),
            row(MdBlock::Rule),
        ];
        let frame = FileFrame {
            path: "README.md".into(),
            language: Some("Markdown".into()),
            top_line: 0,
            total_lines: 5,
            lines: Vec::new(),
            notice: None,
            rendered: Some(rows),
        };
        let json = serde_json::to_string(&frame).unwrap();
        let back: FileFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, frame);
        assert!(!format!("{frame:?}").contains("secret"));
        let rows = frame.rendered.unwrap();
        assert!(!format!("{rows:?}").contains("secret"));
        assert_eq!(rows[1].plain_text(), "[x] secret item");
        assert_eq!(rows[3].plain_text(), "secret cell \u{2502} ");
    }

    #[test]
    fn rf_ctx_02_a_frame_without_rendered_rows_reads_as_before() {
        let json = r#"{"path":"a.rs","language":null,"top_line":0,"total_lines":0,"lines":[],"notice":null}"#;
        let frame: FileFrame = serde_json::from_str(json).unwrap();
        assert_eq!(frame.rendered, None);
        assert!(!serde_json::to_string(&frame).unwrap().contains("rendered"));
    }
}
