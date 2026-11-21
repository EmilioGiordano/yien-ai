//! Markdown to the rows a file preview pane draws when it shows a Markdown
//! file rendered (RF-CTX-02). Pure: text in, rows out, never a panic.

use std::collections::HashMap;
use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use synoptic::Highlighter;
use yien_proto::file_preview::{
    FileLine, MdAlign, MdBlock, MdColumn, MdMarker, MdRow, MdSpan, MdStyle, MdText, SyntaxSpan,
};

use crate::file_preview::{extension_key, highlight_with};

/// Longest text one row carries; a longer paragraph or code line continues
/// on the next rows, so no single row is too big to lay out.
pub(crate) const MAX_ROW_TEXT_BYTES: usize = 2_048;
/// Columns kept per table; cells past them are dropped.
pub(crate) const MAX_TABLE_COLUMNS: usize = 64;
/// Longest text kept per table cell; a longer one ends in an ellipsis.
pub(crate) const MAX_CELL_BYTES: usize = 1_024;
/// The widest a column is sized for, in characters; its cells wrap past it.
pub(crate) const MAX_COLUMN_CHARS: u16 = 60;

const IMAGE_PREFIX: &str = "[image: ";

/// Parses `text` as CommonMark with tables, task lists and strikethrough and
/// returns the rows to draw, in order.
///
/// ```
/// use yien_core::markdown::render;
/// use yien_proto::file_preview::MdBlock;
///
/// let rows = render("# Title\n\nSome *text*.\n");
/// assert!(matches!(rows[0].block, MdBlock::Heading { level: 1, .. }));
/// assert_eq!(rows[1].plain_text(), "Some text.");
/// ```
pub fn render(text: &str) -> Vec<MdRow> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut builder = Builder::default();
    for event in Parser::new_ext(text, options) {
        builder.event(event);
    }
    builder.finish()
}

/// Inline text being collected, with its styled runs.
#[derive(Default)]
struct Inline {
    text: String,
    spans: Vec<MdSpan>,
}

impl Inline {
    fn push(&mut self, s: &str, style: MdStyle) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(s);
        let end = self.text.len();
        if style.is_plain() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.style == style
            && last.end == start
        {
            last.end = end;
        } else {
            self.spans.push(MdSpan { start, end, style });
        }
    }

    fn truncate(&mut self, len: usize) {
        if len >= self.text.len() || !self.text.is_char_boundary(len) {
            return;
        }
        self.text.truncate(len);
        // Spans are sorted, so only the last ones can reach past `len`:
        // walking them all made many empty-alt images quadratic.
        while let Some(last) = self.spans.last_mut() {
            if last.start >= len {
                self.spans.pop();
            } else {
                last.end = last.end.min(len);
                break;
            }
        }
    }

    fn into_text(self) -> MdText {
        MdText {
            text: self.text,
            spans: self.spans,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InlineKind {
    /// A `Paragraph` the parser opened: space follows it.
    Paragraph,
    /// Text straight inside a tight list item.
    Loose,
    Heading(u8),
    Cell,
}

struct CodeBuf {
    lang: Option<&'static str>,
    text: String,
}

#[derive(Default)]
struct TableBuf {
    aligns: Vec<MdAlign>,
    rows: Vec<(Vec<MdText>, bool)>,
    row: Vec<MdText>,
}

#[derive(Default)]
struct Builder {
    rows: Vec<MdRow>,
    quote: usize,
    /// One entry per open list: the next number of an ordered one.
    lists: Vec<Option<u64>>,
    /// The marker of a list item whose first text has not come yet.
    marker: Option<MdMarker>,
    inline: Option<(InlineKind, Inline)>,
    strong: usize,
    emphasis: usize,
    strike: usize,
    link: usize,
    image: usize,
    /// Where each open image's alt text starts in the inline text.
    images: Vec<usize>,
    code: Option<CodeBuf>,
    table: Option<TableBuf>,
    highlighters: HashMap<&'static str, Highlighter>,
}

impl Builder {
    fn style(&self) -> MdStyle {
        MdStyle {
            strong: self.strong > 0,
            emphasis: self.emphasis > 0,
            strike: self.strike > 0,
            code: false,
            link: self.link > 0,
            image: self.image > 0,
        }
    }

    fn depth(n: usize) -> u8 {
        u8::try_from(n).unwrap_or(u8::MAX)
    }

    fn row(&self, block: MdBlock, first: bool, last: bool) -> MdRow {
        MdRow {
            block,
            quote: Self::depth(self.quote),
            indent: Self::depth(self.lists.len()),
            first,
            last,
        }
    }

    fn inline_mut(&mut self) -> &mut Inline {
        &mut self
            .inline
            .get_or_insert_with(|| (InlineKind::Loose, Inline::default()))
            .1
    }

    fn push_text(&mut self, s: &str, style: MdStyle) {
        if let Some(code) = &mut self.code {
            code.text.push_str(s);
            return;
        }
        self.inline_mut().push(s, style);
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text, self.style()),
            Event::Code(text) => {
                let style = MdStyle {
                    code: true,
                    ..self.style()
                };
                self.push_text(&text, style);
            }
            Event::Html(text) | Event::InlineHtml(text) => self.push_text(&text, self.style()),
            Event::SoftBreak => self.push_text(" ", self.style()),
            Event::HardBreak => self.push_text("\n", self.style()),
            Event::Rule => {
                self.close_blocks();
                let row = self.row(MdBlock::Rule, true, true);
                self.rows.push(row);
            }
            Event::TaskListMarker(done) if self.marker.is_some() => {
                self.marker = Some(MdMarker::Task { done });
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.flush_inline();
                self.inline = Some((InlineKind::Paragraph, Inline::default()));
            }
            Tag::Heading { level, .. } => {
                self.close_blocks();
                self.inline = Some((InlineKind::Heading(heading_level(level)), Inline::default()));
            }
            Tag::BlockQuote(_) => {
                self.close_blocks();
                self.quote += 1;
            }
            Tag::CodeBlock(kind) => {
                self.close_blocks();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => fence_language(&info),
                    CodeBlockKind::Indented => None,
                };
                self.code = Some(CodeBuf {
                    lang,
                    text: String::new(),
                });
            }
            Tag::HtmlBlock => {
                self.close_blocks();
                self.code = Some(CodeBuf {
                    lang: None,
                    text: String::new(),
                });
            }
            Tag::List(start) => {
                self.close_blocks();
                self.lists.push(start);
            }
            Tag::Item => {
                self.close_blocks();
                let marker = match self.lists.last_mut() {
                    Some(Some(next)) => {
                        let n = *next;
                        *next = next.saturating_add(1);
                        MdMarker::Number(n)
                    }
                    _ => MdMarker::Bullet,
                };
                self.marker = Some(marker);
            }
            Tag::Table(aligns) => {
                self.close_blocks();
                self.table = Some(TableBuf {
                    aligns: aligns.into_iter().map(align).collect(),
                    ..TableBuf::default()
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => self.inline = Some((InlineKind::Cell, Inline::default())),
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { .. } => self.link += 1,
            Tag::Image { .. } => {
                let style = MdStyle {
                    image: true,
                    ..self.style()
                };
                self.push_text(IMAGE_PREFIX, style);
                self.image += 1;
                let at = self.inline.as_ref().map_or(0, |(_, i)| i.text.len());
                self.images.push(at);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.flush_inline(),
            TagEnd::BlockQuote(_) => {
                self.close_blocks();
                self.quote = self.quote.saturating_sub(1);
                self.mark_last();
            }
            TagEnd::CodeBlock | TagEnd::HtmlBlock => self.flush_code(),
            TagEnd::List(_) => {
                self.close_blocks();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.mark_last();
                }
            }
            TagEnd::Item => self.close_blocks(),
            TagEnd::TableCell => {
                let text = match self.inline.take() {
                    Some((_, inline)) => cap_cell(inline.into_text()),
                    None => MdText::default(),
                };
                if let Some(table) = &mut self.table
                    && table.row.len() < MAX_TABLE_COLUMNS
                {
                    table.row.push(text);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push((row, matches!(tag, TagEnd::TableHead)));
                }
            }
            TagEnd::Table => self.flush_table(),
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = self.link.saturating_sub(1),
            TagEnd::Image => {
                self.image = self.image.saturating_sub(1);
                let style = MdStyle {
                    image: true,
                    ..self.style()
                };
                let at = self.images.pop();
                let inline = self.inline_mut();
                match at {
                    Some(at) if inline.text.len() == at => {
                        inline.truncate(at.saturating_sub(IMAGE_PREFIX.len()));
                        inline.push("[image]", style);
                    }
                    _ => inline.push("]", style),
                }
            }
            _ => {}
        }
    }

    /// Ends whatever inline text or empty list item is open before a new
    /// block starts.
    fn close_blocks(&mut self) {
        self.flush_inline();
        if let Some(marker) = self.marker.take() {
            let row = self.row(
                MdBlock::Item {
                    marker,
                    text: MdText::default(),
                },
                true,
                false,
            );
            self.rows.push(row);
        }
    }

    fn mark_last(&mut self) {
        if let Some(row) = self.rows.last_mut() {
            row.last = true;
        }
    }

    fn flush_inline(&mut self) {
        let Some((kind, inline)) = self.inline.take() else {
            return;
        };
        if kind == InlineKind::Cell {
            return;
        }
        let marker = self.marker.take();
        if inline.text.trim().is_empty() && marker.is_none() {
            return;
        }
        let text = inline.into_text();
        let last = kind != InlineKind::Loose;
        let chunks = split_text(text);
        let count = chunks.len();
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let block = match (idx, kind, marker) {
                (_, InlineKind::Heading(level), _) => MdBlock::Heading { level, text: chunk },
                (0, _, Some(marker)) => MdBlock::Item {
                    marker,
                    text: chunk,
                },
                _ => MdBlock::Paragraph { text: chunk },
            };
            let row = self.row(block, idx == 0, last && idx + 1 == count);
            self.rows.push(row);
        }
    }

    fn flush_code(&mut self) {
        let Some(code) = self.code.take() else {
            return;
        };
        let body = code.text.strip_suffix('\n').unwrap_or(&code.text);
        let raw: Vec<String> = body
            .split('\n')
            .map(|line| {
                let line = line.strip_suffix('\r').unwrap_or(line);
                line.replace('\t', "    ")
            })
            .collect();
        let lines = match code.lang {
            Some(key) => {
                let highlighter = match self.highlighters.entry(key) {
                    std::collections::hash_map::Entry::Occupied(entry) => Some(entry.into_mut()),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        synoptic::from_extension(key, 4).map(|h| entry.insert(h))
                    }
                };
                match highlighter {
                    Some(highlighter) => highlight_with(highlighter, &raw),
                    None => plain(&raw),
                }
            }
            None => plain(&raw),
        };
        let mut pieces = Vec::new();
        for line in lines {
            split_code_line(line, &mut pieces);
        }
        let count = pieces.len();
        for (idx, line) in pieces.into_iter().enumerate() {
            let row = self.row(MdBlock::Code { line }, idx == 0, idx + 1 == count);
            self.rows.push(row);
        }
    }

    fn flush_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let widest_row = table.rows.iter().map(|(r, _)| r.len()).max().unwrap_or(0);
        let count = if table.aligns.is_empty() {
            widest_row
        } else {
            table.aligns.len()
        }
        .min(MAX_TABLE_COLUMNS);
        if count == 0 || table.rows.is_empty() {
            return;
        }
        let mut columns: Vec<MdColumn> = (0..count)
            .map(|i| MdColumn {
                width: 1,
                align: table.aligns.get(i).copied().unwrap_or_default(),
            })
            .collect();
        for (cells, _) in &table.rows {
            for (column, cell) in columns.iter_mut().zip(cells) {
                let chars = cell
                    .text
                    .split('\n')
                    .map(|l| l.chars().count())
                    .max()
                    .unwrap_or(0);
                let chars = u16::try_from(chars)
                    .unwrap_or(u16::MAX)
                    .min(MAX_COLUMN_CHARS);
                column.width = column.width.max(chars);
            }
        }
        let total = table.rows.len();
        for (idx, (mut cells, header)) in table.rows.into_iter().enumerate() {
            cells.resize_with(count, MdText::default);
            let block = MdBlock::TableRow {
                cells,
                columns: columns.clone(),
                header,
            };
            let row = self.row(block, idx == 0, idx + 1 == total);
            self.rows.push(row);
        }
    }

    fn finish(mut self) -> Vec<MdRow> {
        self.flush_code();
        self.flush_table();
        self.close_blocks();
        self.rows
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn align(align: Alignment) -> MdAlign {
    match align {
        Alignment::None => MdAlign::None,
        Alignment::Left => MdAlign::Left,
        Alignment::Center => MdAlign::Center,
        Alignment::Right => MdAlign::Right,
    }
}

/// Fence names that are not a file extension: (lowercase name, extension).
const FENCE_ALIASES: &[(&str, &str)] = &[
    ("rust", "rs"),
    ("python", "py"),
    ("python3", "py"),
    ("javascript", "js"),
    ("node", "js"),
    ("typescript", "ts"),
    ("shell", "sh"),
    ("console", "sh"),
    ("shellsession", "sh"),
    ("powershell", "ps1"),
    ("pwsh", "ps1"),
    ("ps", "ps1"),
    ("golang", "go"),
    ("csharp", "cs"),
    ("c#", "cs"),
    ("kotlin", "kt"),
    ("ruby", "rb"),
    ("nushell", "nu"),
    ("jsonc", "json"),
    ("json5", "json"),
];

/// The highlighter key for a fence's info string (`rust`, `rust,ignore`,
/// `py {.x}`), or `None` when the language is unknown.
fn fence_language(info: &str) -> Option<&'static str> {
    let name = info
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
        .next()?
        .to_ascii_lowercase();
    let ext = FENCE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map_or(name.as_str(), |(_, ext)| ext);
    extension_key(ext)
}

fn plain(raw: &[String]) -> Vec<FileLine> {
    raw.iter()
        .enumerate()
        .map(|(idx, text)| FileLine {
            number: idx + 1,
            text: text.clone(),
            spans: Vec::new(),
        })
        .collect()
}

fn cap_cell(mut text: MdText) -> MdText {
    if text.text.len() <= MAX_CELL_BYTES {
        return text;
    }
    let cut = floor_char_boundary(&text.text, MAX_CELL_BYTES);
    let mut inline = Inline {
        text: std::mem::take(&mut text.text),
        spans: std::mem::take(&mut text.spans),
    };
    inline.truncate(cut);
    inline.push("\u{2026}", MdStyle::default());
    inline.into_text()
}

fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Byte ranges of at most [`MAX_ROW_TEXT_BYTES`] covering `text`, cut after
/// whitespace when `prose` and there is some, else at a character boundary.
fn row_ranges(text: &str, prose: bool) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while text.len() - start > MAX_ROW_TEXT_BYTES {
        let hard = floor_char_boundary(text, start + MAX_ROW_TEXT_BYTES);
        let soft = prose
            .then(|| text[start..hard].rfind(char::is_whitespace))
            .flatten()
            .map(|at| start + at + 1)
            .filter(|&at| at > start && text.is_char_boundary(at));
        let end = soft.unwrap_or(hard);
        ranges.push(start..end);
        start = end;
    }
    ranges.push(start..text.len());
    ranges
}

fn split_text(text: MdText) -> Vec<MdText> {
    if text.text.len() <= MAX_ROW_TEXT_BYTES {
        return vec![text];
    }
    // Spans are sorted and never overlap, so one pass hands each piece its
    // own: filtering every span per piece was quadratic on dense text.
    let mut first = 0;
    row_ranges(&text.text, true)
        .into_iter()
        .map(|range| {
            let mut spans = Vec::new();
            for s in &text.spans[first..] {
                if s.start >= range.end {
                    break;
                }
                let start = s.start.max(range.start);
                let end = s.end.min(range.end);
                if start < end {
                    spans.push(MdSpan {
                        start: start - range.start,
                        end: end - range.start,
                        style: s.style,
                    });
                }
            }
            while text.spans.get(first).is_some_and(|s| s.end <= range.end) {
                first += 1;
            }
            MdText {
                text: text.text[range].to_owned(),
                spans,
            }
        })
        .collect()
}

fn split_code_line(line: FileLine, out: &mut Vec<FileLine>) {
    if line.text.len() <= MAX_ROW_TEXT_BYTES {
        out.push(line);
        return;
    }
    for range in row_ranges(&line.text, false) {
        out.push(FileLine {
            number: line.number,
            text: line.text[range.clone()].to_owned(),
            spans: line
                .spans
                .iter()
                .filter_map(|s| {
                    let start = s.start.max(range.start);
                    let end = s.end.min(range.end);
                    (start < end).then(|| SyntaxSpan {
                        start: start - range.start,
                        end: end - range.start,
                        kind: s.kind,
                    })
                })
                .collect(),
        });
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use yien_proto::file_preview::{FileNotice, PreviewMode, SyntaxKind};

    use super::*;
    use crate::file_preview::{MAX_PREVIEW_BYTES, load_bytes};

    fn texts(rows: &[MdRow]) -> Vec<String> {
        rows.iter().map(MdRow::plain_text).collect()
    }

    fn styled(text: &MdText, pick: fn(MdStyle) -> bool) -> Vec<&str> {
        text.spans
            .iter()
            .filter(|s| pick(s.style))
            .map(|s| &text.text[s.start..s.end])
            .collect()
    }

    fn inline(row: &MdRow) -> &MdText {
        match &row.block {
            MdBlock::Heading { text, .. }
            | MdBlock::Paragraph { text }
            | MdBlock::Item { text, .. } => text,
            other => panic!("no inline text in {other:?}"),
        }
    }

    fn check_fits(text: &str, spans: &[(usize, usize)]) {
        assert!(text.len() <= MAX_ROW_TEXT_BYTES.max(MAX_CELL_BYTES + 3));
        let mut prev = 0;
        for &(start, end) in spans {
            assert!(prev <= start && start < end && end <= text.len());
            assert!(text.is_char_boundary(start) && text.is_char_boundary(end));
            prev = end;
        }
    }

    fn md_spans(text: &MdText) -> Vec<(usize, usize)> {
        text.spans.iter().map(|s| (s.start, s.end)).collect()
    }

    fn assert_rows_fit(rows: &[MdRow]) {
        for row in rows {
            match &row.block {
                MdBlock::Heading { text, .. }
                | MdBlock::Paragraph { text }
                | MdBlock::Item { text, .. } => check_fits(&text.text, &md_spans(text)),
                MdBlock::Code { line } => {
                    let spans: Vec<_> = line.spans.iter().map(|s| (s.start, s.end)).collect();
                    check_fits(&line.text, &spans);
                }
                MdBlock::TableRow { cells, columns, .. } => {
                    assert_eq!(cells.len(), columns.len());
                    for cell in cells {
                        check_fits(&cell.text, &md_spans(cell));
                    }
                }
                MdBlock::Rule => {}
            }
        }
    }

    #[test]
    fn rf_ctx_02_markdown_headings_keep_their_level_and_drop_the_hashes() {
        let rows = render("# One\n\n## Two\n\n###### Six\n\nSetext\n===\n");
        let levels: Vec<u8> = rows
            .iter()
            .map(|r| match r.block {
                MdBlock::Heading { level, .. } => level,
                _ => 0,
            })
            .collect();
        assert_eq!(levels, [1, 2, 6, 1]);
        assert_eq!(texts(&rows), ["One", "Two", "Six", "Setext"]);
        assert!(rows.iter().all(|r| r.first && r.last));
    }

    #[test]
    fn rf_ctx_02_markdown_paragraphs_join_soft_breaks_and_keep_hard_ones() {
        let rows = render("one\ntwo  \nthree\n\nnext\n");
        assert_eq!(texts(&rows), ["one two\nthree", "next"]);
        assert!(rows.iter().all(|r| r.last));
    }

    #[test]
    fn rf_ctx_02_markdown_emphasis_code_strike_and_links_are_styled_runs() {
        let rows = render("a **bold** *it* ~~gone~~ `code` [site](https://x.test) ***both***\n");
        let text = inline(&rows[0]);
        assert_eq!(text.text, "a bold it gone code site both");
        assert_eq!(styled(text, |s| s.strong), ["bold", "both"]);
        assert_eq!(styled(text, |s| s.emphasis), ["it", "both"]);
        assert_eq!(styled(text, |s| s.strike), ["gone"]);
        assert_eq!(styled(text, |s| s.code), ["code"]);
        assert_eq!(styled(text, |s| s.link), ["site"]);
        assert!(!text.text.contains("https"));
    }

    #[test]
    fn rf_ctx_02_markdown_images_are_placeholders_with_their_alt_text() {
        let rows = render("![A cat](cat.png) and ![](x.png)\n");
        let text = inline(&rows[0]);
        assert_eq!(text.text, "[image: A cat] and [image]");
        assert_eq!(styled(text, |s| s.image), ["[image: A cat]", "[image]"]);
        assert!(!text.text.contains("png"));
    }

    #[test]
    fn rf_ctx_02_markdown_fenced_code_is_highlighted_by_its_language() {
        let rows = render("```rust\nfn main() {\n\tlet s = \"hi\";\n}\n```\n\n```\nplain\n```\n");
        let lines: Vec<&FileLine> = rows
            .iter()
            .filter_map(|r| match &r.block {
                MdBlock::Code { line } => Some(line),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1].text, "    let s = \"hi\";");
        assert!(lines[0].spans.iter().any(|s| s.kind == SyntaxKind::Keyword));
        assert!(lines[1].spans.iter().any(|s| s.kind == SyntaxKind::String));
        assert!(lines[3].spans.is_empty());
        let numbers: Vec<usize> = lines.iter().map(|l| l.number).collect();
        assert_eq!(numbers, [1, 2, 3, 1]);
        let flags: Vec<(bool, bool)> = rows.iter().map(|r| (r.first, r.last)).collect();
        assert_eq!(
            flags,
            [(true, false), (false, false), (false, true), (true, true)]
        );
    }

    #[test]
    fn rf_ctx_02_markdown_fence_names_map_to_highlighters() {
        assert_eq!(fence_language("rust"), Some("rs"));
        assert_eq!(fence_language("rust,ignore"), Some("rs"));
        assert_eq!(fence_language("Python {.x}"), Some("py"));
        assert_eq!(fence_language("toml"), Some("toml"));
        assert_eq!(fence_language("pwsh"), Some("sh"));
        assert_eq!(fence_language("brainfuck"), None);
        assert_eq!(fence_language(""), None);
    }

    #[test]
    fn rf_ctx_02_markdown_an_unterminated_fence_runs_to_the_end() {
        let rows = render("Intro\n\n```rust\nfn a() {}\n\n# not a heading\n");
        assert_eq!(texts(&rows), ["Intro", "fn a() {}", "", "# not a heading"]);
        assert!(matches!(rows[3].block, MdBlock::Code { .. }));
        assert!(rows[3].last);
    }

    #[test]
    fn rf_ctx_02_markdown_indented_code_and_html_blocks_show_as_code() {
        let rows = render("    indented\n\n<div>\nhi\n</div>\n");
        assert!(rows.iter().all(|r| matches!(r.block, MdBlock::Code { .. })));
        assert_eq!(texts(&rows), ["indented", "<div>", "hi", "</div>"]);
    }

    #[test]
    fn rf_ctx_02_markdown_lists_number_nest_and_check_tasks() {
        let src = "- one\n- two\n  1. inner\n  2. next\n     - deep\n- [x] done\n- [ ] todo\n\n7. seven\n8. eight\n";
        let rows = render(src);
        assert_eq!(
            texts(&rows),
            [
                "\u{2022} one",
                "\u{2022} two",
                "1. inner",
                "2. next",
                "\u{2022} deep",
                "[x] done",
                "[ ] todo",
                "7. seven",
                "8. eight",
            ]
        );
        let indents: Vec<u8> = rows.iter().map(|r| r.indent).collect();
        assert_eq!(indents, [1, 1, 2, 2, 3, 1, 1, 1, 1]);
        // Tight items sit together; space follows each whole list.
        let last: Vec<bool> = rows.iter().map(|r| r.last).collect();
        assert_eq!(
            last,
            [false, false, false, false, false, false, true, false, true]
        );
    }

    #[test]
    fn rf_ctx_02_markdown_loose_items_keep_their_paragraphs_and_code() {
        let rows = render("1. first\n\n   more text\n\n   ```\n   code\n   ```\n2. second\n");
        assert_eq!(texts(&rows), ["1. first", "more text", "code", "2. second"]);
        assert!(rows.iter().all(|r| r.indent == 1));
        assert!(matches!(rows[1].block, MdBlock::Paragraph { .. }));
        assert!(matches!(rows[2].block, MdBlock::Code { .. }));
    }

    #[test]
    fn rf_ctx_02_markdown_an_item_without_text_keeps_its_marker() {
        let rows = render("-\n- \n  - nested\n");
        assert_eq!(texts(&rows), ["\u{2022} ", "\u{2022} ", "\u{2022} nested"]);
    }

    #[test]
    fn rf_ctx_02_markdown_block_quotes_count_their_depth() {
        let rows = render("> quoted\n>\n> > deeper\n\nafter\n");
        let depths: Vec<u8> = rows.iter().map(|r| r.quote).collect();
        assert_eq!(depths, [1, 2, 0]);
        assert_eq!(texts(&rows), ["quoted", "deeper", "after"]);
    }

    #[test]
    fn rf_ctx_02_markdown_tables_align_columns_across_rows() {
        let src = "| Crate | Layer | Owns |\n|:---|:---:|---:|\n| `yien-proto` | 0 | Messages |\n| `yien-ui` | 3 | The GPUI window |\n";
        let rows = render(src);
        assert_eq!(rows.len(), 3);
        for (idx, row) in rows.iter().enumerate() {
            let MdBlock::TableRow {
                cells,
                columns,
                header,
            } = &row.block
            else {
                panic!("not a table row");
            };
            assert_eq!(*header, idx == 0);
            assert_eq!(cells.len(), 3);
            let widths: Vec<u16> = columns.iter().map(|c| c.width).collect();
            assert_eq!(widths, [10, 5, 15]);
            let aligns: Vec<MdAlign> = columns.iter().map(|c| c.align).collect();
            assert_eq!(aligns, [MdAlign::Left, MdAlign::Center, MdAlign::Right]);
            assert_eq!((row.first, row.last), (idx == 0, idx == 2));
        }
        let MdBlock::TableRow { cells, .. } = &rows[1].block else {
            unreachable!()
        };
        assert_eq!(styled(&cells[0], |s| s.code), ["yien-proto"]);
    }

    #[test]
    fn rf_ctx_02_markdown_tables_with_uneven_rows_pad_or_drop_cells() {
        let src = "| a | b | c |\n|---|---|---|\n| 1 |\n| 1 | 2 | 3 | 4 | 5 |\n";
        let rows = render(src);
        assert_eq!(rows.len(), 3);
        for row in &rows {
            let MdBlock::TableRow { cells, columns, .. } = &row.block else {
                panic!("not a table row");
            };
            assert_eq!((cells.len(), columns.len()), (3, 3));
        }
        assert_eq!(texts(&rows)[1], "1 \u{2502}  \u{2502} ");
        assert_eq!(texts(&rows)[2], "1 \u{2502} 2 \u{2502} 3");
    }

    #[test]
    fn rf_ctx_02_markdown_huge_cells_and_wide_tables_are_capped() {
        let wide: String = (0..MAX_TABLE_COLUMNS + 10)
            .map(|i| format!("| c{i} "))
            .collect();
        let rule: String = "|---".repeat(MAX_TABLE_COLUMNS + 10);
        let long = "x".repeat(MAX_CELL_BYTES * 3);
        let src = format!("{wide}|\n{rule}|\n| {long} |\n");
        let rows = render(&src);
        let MdBlock::TableRow { cells, columns, .. } = &rows[1].block else {
            panic!("not a table row");
        };
        assert_eq!(cells.len(), MAX_TABLE_COLUMNS);
        assert_eq!(columns[0].width, MAX_COLUMN_CHARS);
        assert!(cells[0].text.ends_with('\u{2026}'));
        assert!(cells[0].text.len() <= MAX_CELL_BYTES + 3);
        assert_rows_fit(&rows);
    }

    #[test]
    fn rf_ctx_02_markdown_rules_are_rows_of_their_own() {
        let rows = render("above\n\n---\n\n***\nbelow\n");
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[1].block, MdBlock::Rule));
        assert!(matches!(rows[2].block, MdBlock::Rule));
    }

    #[test]
    fn rf_ctx_02_markdown_long_paragraphs_and_code_lines_split_into_rows() {
        let words = "word ".repeat(2_000);
        let code = "y".repeat(MAX_ROW_TEXT_BYTES * 2 + 5);
        let rows = render(&format!("**{}**\n\n```\n{code}\n```\n", words.trim_end()));
        let paragraphs: Vec<&MdRow> = rows
            .iter()
            .filter(|r| matches!(r.block, MdBlock::Paragraph { .. }))
            .collect();
        assert_eq!(paragraphs.len(), 5);
        assert!(paragraphs[0].first && !paragraphs[1].first);
        assert!(paragraphs[4].last && !paragraphs[3].last);
        let joined: String = paragraphs.iter().map(|r| inline(r).text.as_str()).collect();
        assert_eq!(joined, words.trim_end());
        // Each piece stays bold and is cut after a space.
        for row in &paragraphs[..4] {
            let text = inline(row);
            assert!(text.text.ends_with(' '));
            assert_eq!(styled(text, |s| s.strong), [text.text.as_str()]);
        }
        let code_rows = rows
            .iter()
            .filter(|r| matches!(r.block, MdBlock::Code { .. }))
            .count();
        assert_eq!(code_rows, 3);
        assert_rows_fit(&rows);
    }

    #[test]
    fn rf_ctx_02_markdown_many_empty_alt_images_load_in_linear_time() {
        // 190,000 images in one paragraph, each leaving its own span: the
        // quadratic version walked ~1.8e10 spans here.
        let src = "![](x) ".repeat(190_000);
        assert!(src.len() < crate::file_preview::MAX_PREVIEW_BYTES);
        let started = std::time::Instant::now();
        let rows = render(&src);
        let elapsed = started.elapsed();
        assert!(elapsed.as_secs() < 30, "took {elapsed:?}");
        let images: usize = rows
            .iter()
            .map(|r| styled(inline(r), |s| s.image).len())
            .sum();
        assert!(images >= 190_000, "{images}");
        assert!(rows.iter().all(|r| !inline(r).text.contains("[image: ")));
        assert_rows_fit(&rows);
    }

    #[test]
    fn rf_ctx_02_markdown_a_dense_two_megabyte_paragraph_splits_in_linear_time() {
        let src = "**a** ".repeat(330_000);
        assert!(src.len() < crate::file_preview::MAX_PREVIEW_BYTES);
        let started = std::time::Instant::now();
        let rows = render(&src);
        let elapsed = started.elapsed();
        assert!(elapsed.as_secs() < 30, "took {elapsed:?}");
        assert!(rows.len() > 300);
        let bold: usize = rows
            .iter()
            .map(|r| styled(inline(r), |s| s.strong).len())
            .sum();
        assert_eq!(bold, 330_000);
        assert_rows_fit(&rows);
    }

    #[test]
    fn rf_ctx_02_markdown_deep_nesting_never_overflows() {
        let quotes = format!("{} deep\n", ">".repeat(5_000));
        let lists: String = (0..400)
            .map(|i| format!("{}- item\n", "  ".repeat(i)))
            .collect();
        let brackets = "[".repeat(50_000);
        for src in [quotes, lists, brackets] {
            assert_rows_fit(&render(&src));
        }
        let rows = render(&format!("{} deep\n", ">".repeat(300)));
        assert_eq!(rows[0].quote, u8::MAX);
    }

    #[test]
    fn rf_ctx_02_markdown_a_one_megabyte_file_renders_within_the_cap() {
        // Long lines, so the file stays under the line cap as well.
        let words = "Some *text* with `code` and a [link](x). ".repeat(45);
        let block = format!(
            "## Section\n\n{words}\n\n- a\n- b\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn f() {{}}\n```\n\n"
        );
        let src = block.repeat(1024 * 1024 / block.len() + 1);
        assert!(src.len() >= 1024 * 1024 && src.len() < MAX_PREVIEW_BYTES);
        let doc = load_bytes("README.md", src.as_bytes(), src.len() as u64, false);
        assert_eq!(doc.notice, None, "a 1 MB file is inside both caps");
        let rows = doc.rendered.as_ref().unwrap();
        assert!(rows.len() > 3_000);
        assert_rows_fit(rows);
        let frame = doc.frame_in(PreviewMode::Rendered, 0, 40);
        assert_eq!(frame.rendered.unwrap().len(), 40);
        assert!(frame.lines.is_empty());
    }

    #[test]
    fn rf_ctx_02_markdown_a_capped_file_renders_the_prefix_the_source_shows() {
        let src = "# Title\n\nbody\n".repeat(10_000);
        let doc = load_bytes("big.md", src.as_bytes(), src.len() as u64, false);
        assert!(matches!(
            doc.notice,
            Some(FileNotice::Truncated {
                capped_by_lines: true,
                ..
            })
        ));
        // 20,000 lines hold 6,667 headings and 6,666 paragraphs.
        assert_eq!(doc.rendered.as_ref().unwrap().len(), 13_333);
    }

    #[test]
    fn rf_ctx_02_markdown_invalid_utf8_renders_like_the_source_view() {
        let bytes = b"# Title\n\n\xff\xfe broken text\n";
        let doc = load_bytes("notes.md", bytes, bytes.len() as u64, false);
        assert!(doc.lines[2].text.contains("broken text"));
        let rows = doc.rendered.clone().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(inline(&rows[1]).text, doc.lines[2].text);
    }

    #[test]
    fn rf_ctx_02_markdown_only_markdown_files_get_rendered_rows() {
        let src = b"# Title\n";
        for (path, rendered) in [
            ("README.md", true),
            ("docs/Guide.MARKDOWN", true),
            ("notes.txt", false),
            ("src/lib.rs", false),
        ] {
            let doc = load_bytes(path, src, src.len() as u64, false);
            assert_eq!(doc.rendered.is_some(), rendered, "{path}");
        }
        let bin = load_bytes("x.md", b"\x00\x01", 2, false);
        assert_eq!(bin.rendered, None);
        let empty = load_bytes("empty.md", b"", 0, false);
        assert_eq!(empty.rendered, Some(Vec::new()));
        let frame = empty.frame_in(PreviewMode::Rendered, 0, 10);
        assert_eq!(frame.total_lines, 0);
        assert_eq!(frame.rendered, Some(Vec::new()));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn rf_ctx_02_markdown_arbitrary_text_never_panics(
            src in "(\\PC|[#>*_`~|\\-\\[\\]()!:\n ]){0,400}",
        ) {
            assert_rows_fit(&render(&src));
        }

        #[test]
        fn rf_ctx_02_markdown_arbitrary_bytes_never_panic(
            bytes in prop::collection::vec(any::<u8>(), 0..2048),
            top in 0usize..200,
            rows in 1usize..60,
        ) {
            let doc = load_bytes("fuzz.md", &bytes, bytes.len() as u64, false);
            if let Some(rendered) = &doc.rendered {
                assert_rows_fit(rendered);
            }
            let frame = doc.frame_in(PreviewMode::Rendered, top, rows);
            let shown = frame.rendered.as_ref().map_or(frame.lines.len(), Vec::len);
            prop_assert!(shown <= rows);
        }
    }
}
