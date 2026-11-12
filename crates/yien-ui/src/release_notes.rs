//! A release's notes (GitHub markdown, untrusted network input) as a flat
//! list of blocks the window can draw. The reader is line oriented and
//! linear: each line is read once, each inline byte a fixed number of
//! times, nothing recurses and nothing backtracks, so no input can make it
//! slow or deep. It reads a subset: ATX headings, paragraphs, bullet and
//! numbered lists (deeper than [`MAX_DEPTH`] is drawn at that depth),
//! fenced code, one level of quote, rules, and inline bold, italic,
//! strikethrough, code and links. Raw HTML stays text, an image is its alt
//! text, and nothing is ever loaded; a link is only a target the user may
//! click.

use std::collections::HashMap;

/// The most characters of notes read; the rest is cut.
pub(crate) const NOTES_LIMIT: usize = 64 * 1024;

/// The deepest list level drawn; deeper items are drawn at this one.
pub(crate) const MAX_DEPTH: u8 = 3;

/// A run of text with one style.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) text: String,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) strike: bool,
    pub(crate) code: bool,
    /// Where a click goes; only `http` and `https` targets are kept.
    pub(crate) link: Option<String>,
}

impl Span {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    fn same_style(&self, other: &Span) -> bool {
        (self.bold, self.italic, self.strike, self.code, &self.link)
            == (
                other.bold,
                other.italic,
                other.strike,
                other.code,
                &other.link,
            )
    }
}

/// What a block is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `#` to `######`.
    Heading(u8),
    Paragraph,
    /// A list item at `depth` (0 to [`MAX_DEPTH`]) with its marker: `•` or
    /// the number as written, such as `3.`.
    Item {
        depth: u8,
        marker: String,
    },
    /// Fenced code, shown verbatim in one span.
    Code,
    Rule,
}

/// One block of the notes; blocks never hold blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Block {
    pub(crate) kind: Kind,
    pub(crate) spans: Vec<Span>,
    /// Inside a `>` quote.
    pub(crate) quote: bool,
}

/// Whether a link target may be opened in the browser: `http` or `https`
/// only, so a click can never run a local file or another URL scheme.
pub(crate) fn openable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && lower.len() > "https://".len()
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// The notes as blocks.
pub(crate) fn parse(notes: &str) -> Vec<Block> {
    let notes = cut(notes);
    let mut reader = Reader::default();
    let mut lines = notes.lines();
    while let Some(line) = lines.next() {
        let (quote, line) = strip_quote(line);
        if let Some((fence, len)) = fence_open(line) {
            reader.flush();
            reader.levels.clear();
            let mut code = String::new();
            for inner in lines.by_ref() {
                let inner = if quote { strip_quote(inner).1 } else { inner };
                if fence_close(inner, fence, len) {
                    break;
                }
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(inner);
            }
            reader.blocks.push(Block {
                kind: Kind::Code,
                spans: vec![Span::plain(code)],
                quote,
            });
            continue;
        }
        reader.line(line, quote);
    }
    reader.flush();
    reader.blocks
}

fn cut(notes: &str) -> String {
    match notes.char_indices().nth(NOTES_LIMIT) {
        Some((at, _)) => format!("{}\n\n…", &notes[..at]),
        None => notes.to_owned(),
    }
}

fn indent(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

/// One level of `>`; a deeper `>` stays in the text.
fn strip_quote(line: &str) -> (bool, &str) {
    let lead = indent(line);
    if lead <= 3 && line[lead..].starts_with('>') {
        let rest = &line[lead + 1..];
        (true, rest.strip_prefix(' ').unwrap_or(rest))
    } else {
        (false, line)
    }
}

fn fence_open(line: &str) -> Option<(u8, usize)> {
    let lead = indent(line);
    if lead > 3 {
        return None;
    }
    let rest = &line.as_bytes()[lead..];
    let fence = *rest.first()?;
    if fence != b'`' && fence != b'~' {
        return None;
    }
    let len = rest.iter().take_while(|b| **b == fence).count();
    let info = &rest[len..];
    (len >= 3 && !(fence == b'`' && info.contains(&b'`'))).then_some((fence, len))
}

fn fence_close(line: &str, fence: u8, len: usize) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= len && trimmed.bytes().all(|b| b == fence)
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let lead = indent(line);
    if lead > 3 {
        return None;
    }
    let rest = &line[lead..];
    let level = rest.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let text = &rest[level..];
    if !(text.is_empty() || text.starts_with([' ', '\t'])) {
        return None;
    }
    let text = text.trim().trim_end_matches('#').trim_end();
    Some((level as u8, text))
}

fn rule(line: &str) -> bool {
    let mut kind = None;
    let mut count = 0;
    for b in line.bytes() {
        match b {
            b' ' | b'\t' => {}
            b'-' | b'*' | b'_' if kind.is_none_or(|k| k == b) => {
                kind = Some(b);
                count += 1;
            }
            _ => return false,
        }
    }
    count >= 3
}

/// A list item: its indent in columns, marker and text.
fn item(line: &str) -> Option<(usize, String, &str)> {
    let lead = line
        .bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .map(|b| if b == b'\t' { 4 } else { 1 })
        .sum::<usize>();
    let rest = line.trim_start_matches([' ', '\t']);
    let bytes = rest.as_bytes();
    let (marker, after) = match bytes.first()? {
        b'-' | b'*' | b'+' => ("•".to_owned(), 1),
        b'0'..=b'9' => {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 9 || !matches!(bytes.get(digits), Some(b'.' | b')')) {
                return None;
            }
            (format!("{}.", &rest[..digits]), digits + 1)
        }
        _ => return None,
    };
    match bytes.get(after) {
        Some(b' ' | b'\t') => Some((lead, marker, rest[after..].trim_start())),
        None => Some((lead, marker, "")),
        _ => None,
    }
}

/// Collects lines into blocks.
#[derive(Default)]
struct Reader {
    blocks: Vec<Block>,
    /// The block being filled: kind, quote and raw text so far.
    open: Option<(Kind, bool, String)>,
    /// Indents of the open list levels, outermost first.
    levels: Vec<usize>,
}

impl Reader {
    fn line(&mut self, line: &str, quote: bool) {
        if line.trim().is_empty() {
            self.flush();
            return;
        }
        if let Some((level, text)) = heading(line) {
            self.flush();
            self.levels.clear();
            self.push(Kind::Heading(level), quote, text);
            return;
        }
        if rule(line) {
            self.flush();
            self.levels.clear();
            self.blocks.push(Block {
                kind: Kind::Rule,
                spans: Vec::new(),
                quote,
            });
            return;
        }
        if let Some((lead, marker, text)) = item(line) {
            self.flush();
            let depth = self.depth(lead);
            self.open = Some((Kind::Item { depth, marker }, quote, text.to_owned()));
            return;
        }
        match &mut self.open {
            // A line that starts no block continues the open one.
            Some((_, _, text)) => {
                if text.ends_with("  ") {
                    text.truncate(text.trim_end().len());
                    text.push('\n');
                } else {
                    text.push(' ');
                }
                text.push_str(line.trim_start());
            }
            None => {
                if indent(line) < 2 {
                    self.levels.clear();
                }
                self.open = Some((Kind::Paragraph, quote, line.trim_start().to_owned()));
            }
        }
    }

    /// The level of an item indented by `lead`, capped at [`MAX_DEPTH`].
    fn depth(&mut self, lead: usize) -> u8 {
        while self.levels.last().is_some_and(|l| *l > lead) {
            self.levels.pop();
        }
        if self.levels.last() != Some(&lead) {
            self.levels.push(lead);
        }
        u8::try_from(self.levels.len() - 1)
            .unwrap_or(MAX_DEPTH)
            .min(MAX_DEPTH)
    }

    fn push(&mut self, kind: Kind, quote: bool, text: &str) {
        self.blocks.push(Block {
            kind,
            spans: inline(text.trim_end()),
            quote,
        });
    }

    fn flush(&mut self) {
        if let Some((kind, quote, text)) = self.open.take() {
            self.push(kind, quote, &text);
        }
    }
}

/// An inline delimiter: bold, italic or strikethrough, as a counter slot.
const BOLD: usize = 0;
const ITALIC: usize = 1;
const STRIKE: usize = 2;

/// One piece of a line, before delimiters are paired.
enum Token<'a> {
    Text(&'a str),
    Code(&'a str),
    Link(&'a str, String),
    /// A delimiter of a slot, its literal text, and whether it may open and
    /// close.
    Delim {
        slot: usize,
        text: &'a str,
        open: bool,
        close: bool,
    },
}

/// For each position `i`, the first position at or after `i` whose byte
/// passes `hit`, or the length; one backward pass.
fn next_where(bytes: &[u8], hit: impl Fn(u8) -> bool) -> Vec<usize> {
    let mut next = vec![bytes.len(); bytes.len() + 1];
    for i in (0..bytes.len()).rev() {
        next[i] = if hit(bytes[i]) { i } else { next[i + 1] };
    }
    next
}

/// Inline markup in one left-to-right pass. Code spans, links, images and
/// autolinks find their ends through precomputed "next" tables and
/// cursors that only move forward; delimiters are paired afterwards with a
/// stack per kind. Anything unmatched is literal text.
fn inline(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let next_close = next_where(bytes, |b| b == b']');
    let next_open = next_where(bytes, |b| b == b'[');
    let next_paren = next_where(bytes, |b| b == b')');
    let next_gt = next_where(bytes, |b| b == b'>');
    let next_lt = next_where(bytes, |b| b == b'<');
    let next_space = next_where(bytes, |b| b.is_ascii_whitespace());
    // Backtick runs by length; the closer of a run is the next run of the
    // same length, found by a cursor per length that only moves forward.
    let mut by_len: HashMap<usize, (Vec<usize>, usize)> = HashMap::new();
    let mut i = 0;
    while i < n {
        if bytes[i] == b'`' {
            let start = i;
            while i < n && bytes[i] == b'`' {
                i += 1;
            }
            by_len.entry(i - start).or_default().0.push(start);
        } else {
            i += 1;
        }
    }

    let mut tokens: Vec<Token> = Vec::new();
    let mut plain = 0;
    let mut i = 0;
    while i < n {
        let b = bytes[i];
        match b {
            b'\\' if i + 1 < n && bytes[i + 1].is_ascii_punctuation() => {
                flush(&mut tokens, text, plain, i);
                tokens.push(Token::Text(&text[i + 1..i + 2]));
                i += 2;
                plain = i;
            }
            b'`' => {
                let len = bytes[i..].iter().take_while(|c| **c == b'`').count();
                let close = by_len.get_mut(&len).and_then(|(starts, cursor)| {
                    while starts.get(*cursor).is_some_and(|s| *s <= i) {
                        *cursor += 1;
                    }
                    starts.get(*cursor).copied()
                });
                match close {
                    Some(end) => {
                        flush(&mut tokens, text, plain, i);
                        let inner = &text[i + len..end];
                        let inner = match inner.strip_prefix(' ').and_then(|s| s.strip_suffix(' '))
                        {
                            Some(stripped) if !stripped.trim().is_empty() => stripped,
                            _ => inner,
                        };
                        tokens.push(Token::Code(inner));
                        i = end + len;
                        plain = i;
                    }
                    None => i += len,
                }
            }
            b'!' | b'[' => {
                let image = b == b'!';
                let open = if image { i + 1 } else { i };
                if bytes.get(open) != Some(&b'[') {
                    i += 1;
                    continue;
                }
                let close = next_close[open + 1];
                let target = (close < n
                    && next_open[open + 1] > close
                    && bytes.get(close + 1) == Some(&b'('))
                .then(|| (close + 2, next_paren[close + 2]))
                .filter(|(from, to)| *to < n && next_space[*from] >= *to);
                match target {
                    Some((from, to)) => {
                        flush(&mut tokens, text, plain, i);
                        let label = &text[open + 1..close];
                        let url = &text[from..to];
                        if !image && openable(url) {
                            tokens.push(Token::Link(label, url.to_owned()));
                        } else {
                            tokens.push(Token::Text(label));
                        }
                        i = to + 1;
                        plain = i;
                    }
                    None => i = open + 1,
                }
            }
            b'<' => {
                let end = next_gt[i + 1];
                let inner = &text[i + 1..end];
                if end < n
                    && next_lt[i + 1] > end
                    && next_space[i + 1] > end
                    && (inner.starts_with("https://") || inner.starts_with("http://"))
                    && openable(inner)
                {
                    flush(&mut tokens, text, plain, i);
                    tokens.push(Token::Link(inner, inner.to_owned()));
                    i = end + 1;
                    plain = i;
                } else {
                    i += 1;
                }
            }
            b'*' | b'_' | b'~' => {
                let len = bytes[i..].iter().take_while(|c| **c == b).count();
                let before = text[..i].chars().next_back();
                let after = text[i + len..].chars().next();
                let space = |c: Option<char>| c.is_none_or(char::is_whitespace);
                let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
                let (mut open, mut close) = (!space(after), !space(before));
                if b == b'_' {
                    open &= !word(before);
                    close &= !word(after);
                }
                flush(&mut tokens, text, plain, i);
                let run = &text[i..i + len];
                if b == b'~' {
                    tokens.push(if len == 2 {
                        Token::Delim {
                            slot: STRIKE,
                            text: run,
                            open,
                            close,
                        }
                    } else {
                        Token::Text(run)
                    });
                } else {
                    for k in 0..len / 2 {
                        tokens.push(Token::Delim {
                            slot: BOLD,
                            text: &run[2 * k..2 * k + 2],
                            open,
                            close,
                        });
                    }
                    if len % 2 == 1 {
                        tokens.push(Token::Delim {
                            slot: ITALIC,
                            text: &run[len - 1..],
                            open,
                            close,
                        });
                    }
                }
                i += len;
                plain = i;
            }
            _ => i += 1,
        }
    }
    flush(&mut tokens, text, plain, n);
    style(tokens)
}

/// Adds the plain text between `from` and `to`, if any.
fn flush<'a>(tokens: &mut Vec<Token<'a>>, text: &'a str, from: usize, to: usize) {
    if from < to {
        tokens.push(Token::Text(&text[from..to]));
    }
}

/// What a paired delimiter does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Literal,
    Opens,
    Closes,
}

/// Pairs delimiters with a stack per kind, then sweeps once with a counter
/// per kind; an unpaired delimiter stays text.
fn style(tokens: Vec<Token>) -> Vec<Span> {
    let mut roles = vec![Role::Literal; tokens.len()];
    let mut stacks: [Vec<usize>; 3] = Default::default();
    for (at, token) in tokens.iter().enumerate() {
        if let Token::Delim {
            slot, open, close, ..
        } = token
        {
            if *close && let Some(opener) = stacks[*slot].pop() {
                roles[opener] = Role::Opens;
                roles[at] = Role::Closes;
            } else if *open {
                stacks[*slot].push(at);
            }
        }
    }
    let mut depth = [0usize; 3];
    let mut out = Vec::new();
    for (token, role) in tokens.into_iter().zip(roles) {
        let style = Span {
            bold: depth[BOLD] > 0,
            italic: depth[ITALIC] > 0,
            strike: depth[STRIKE] > 0,
            ..Span::default()
        };
        let span = match token {
            Token::Delim { slot, .. } if role == Role::Opens => {
                depth[slot] += 1;
                continue;
            }
            Token::Delim { slot, .. } if role == Role::Closes => {
                depth[slot] = depth[slot].saturating_sub(1);
                continue;
            }
            Token::Delim { text, .. } | Token::Text(text) => Span {
                text: text.to_owned(),
                ..style
            },
            Token::Code(text) => Span {
                text: text.to_owned(),
                code: true,
                ..style
            },
            Token::Link(text, url) => Span {
                text: text.to_owned(),
                link: Some(url),
                ..style
            },
        };
        push(&mut out, span);
    }
    out
}

/// Appends a span, joining it to the last one when they look the same.
fn push(out: &mut Vec<Span>, span: Span) {
    if span.text.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.same_style(&span) => last.text.push_str(&span.text),
        _ => out.push(span),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn text(s: &str) -> Span {
        Span::plain(s)
    }

    fn block(kind: Kind, spans: Vec<Span>) -> Block {
        Block {
            kind,
            spans,
            quote: false,
        }
    }

    fn para(s: &str) -> Block {
        block(Kind::Paragraph, vec![text(s)])
    }

    fn item(depth: u8, marker: &str, s: &str) -> Block {
        block(
            Kind::Item {
                depth,
                marker: marker.into(),
            },
            vec![text(s)],
        )
    }

    #[test]
    fn headings_paragraphs_and_inline_styles() {
        let blocks =
            parse("# YIEN 0.1.0\n\nThe **first** *pre-release*, with `code` and ~~gone~~.\n");
        assert_eq!(
            blocks,
            vec![
                block(Kind::Heading(1), vec![text("YIEN 0.1.0")]),
                block(
                    Kind::Paragraph,
                    vec![
                        text("The "),
                        Span {
                            bold: true,
                            ..text("first")
                        },
                        text(" "),
                        Span {
                            italic: true,
                            ..text("pre-release")
                        },
                        text(", with "),
                        Span {
                            code: true,
                            ..text("code")
                        },
                        text(" and "),
                        Span {
                            strike: true,
                            ..text("gone")
                        },
                        text("."),
                    ]
                ),
            ]
        );
    }

    #[test]
    fn nested_bold_italic_keeps_both_and_unmatched_stays_text() {
        assert_eq!(
            parse("***both***"),
            vec![block(
                Kind::Paragraph,
                vec![Span {
                    bold: true,
                    italic: true,
                    ..text("both")
                }]
            )]
        );
        assert_eq!(parse("a ** b * c _d"), vec![para("a ** b * c _d")]);
        assert_eq!(parse("snake_case_name"), vec![para("snake_case_name")]);
        assert_eq!(parse("\\*not\\*"), vec![para("*not*")]);
        assert_eq!(
            parse("`` a ` b ``"),
            vec![block(
                Kind::Paragraph,
                vec![Span {
                    code: true,
                    ..text("a ` b")
                }]
            )]
        );
        assert_eq!(parse("`open only"), vec![para("`open only")]);
    }

    #[test]
    fn lists_bulleted_numbered_nested_and_capped() {
        let blocks = parse("- one\n- two\n  1. inner\n  2. more\n\n3. three\n4) four\n");
        assert_eq!(
            blocks,
            vec![
                item(0, "•", "one"),
                item(0, "•", "two"),
                item(1, "1.", "inner"),
                item(1, "2.", "more"),
                item(0, "3.", "three"),
                item(0, "4.", "four"),
            ]
        );
        let deep: String = (0..8)
            .map(|d| format!("{}- l{d}\n", "  ".repeat(d)))
            .collect();
        let depths: Vec<u8> = parse(&deep)
            .iter()
            .map(|b| match b.kind {
                Kind::Item { depth, .. } => depth,
                _ => 99,
            })
            .collect();
        assert_eq!(depths, [0, 1, 2, 3, 3, 3, 3, 3]);
        assert_eq!(
            parse("- [x] done\n- [ ] todo\n"),
            vec![item(0, "•", "[x] done"), item(0, "•", "[ ] todo")]
        );
        assert_eq!(
            parse("- first line\n  goes on\n"),
            vec![item(0, "•", "first line goes on")]
        );
    }

    #[test]
    fn code_blocks_keep_their_text_verbatim() {
        let blocks = parse(
            "```powershell\nGet-FileHash .\\yien.exe -Algorithm SHA256\n<b>x</b> **no**\n```\nafter\n",
        );
        assert_eq!(
            blocks,
            vec![
                block(
                    Kind::Code,
                    vec![text(
                        "Get-FileHash .\\yien.exe -Algorithm SHA256\n<b>x</b> **no**"
                    )]
                ),
                para("after"),
            ]
        );
        assert_eq!(
            parse("~~~\nunclosed"),
            vec![block(Kind::Code, vec![text("unclosed")])]
        );
    }

    #[test]
    fn raw_html_is_shown_as_text_never_rendered() {
        let blocks =
            parse("<script>alert(1)</script>\n\nSee <b>this</b> <img src=\"https://x/y.png\">.\n");
        assert_eq!(blocks[0], para("<script>alert(1)</script>"));
        assert_eq!(
            blocks[1],
            para("See <b>this</b> <img src=\"https://x/y.png\">.")
        );
    }

    #[test]
    fn images_become_their_alt_text_and_load_nothing() {
        let blocks = parse("![the new tab bar](https://example.com/shot.png)\n");
        assert_eq!(blocks, vec![para("the new tab bar")]);
        assert!(!format!("{blocks:?}").contains("example.com"));
    }

    #[test]
    fn links_keep_only_web_targets() {
        let blocks = parse(
            "[page](https://github.com/EmilioGiordano/yien-ai) [local](file:///C:/Windows/System32/calc.exe) \
             [js](javascript:alert(1)) <https://example.com/auto> [a [b](https://x.y/z) [sp](https://a b)",
        );
        let spans = &blocks[0].spans;
        let link = |label: &str| {
            spans
                .iter()
                .find(|s| s.text.contains(label))
                .unwrap_or_else(|| panic!("{label} in {spans:?}"))
                .link
                .clone()
        };
        assert_eq!(
            link("page").as_deref(),
            Some("https://github.com/EmilioGiordano/yien-ai")
        );
        assert_eq!(link("local"), None);
        assert_eq!(link("js"), None);
        assert_eq!(
            link("https://example.com/auto").as_deref(),
            Some("https://example.com/auto")
        );
        assert_eq!(
            link("b").as_deref(),
            Some("https://x.y/z"),
            "the inner bracket"
        );
        assert_eq!(link("sp"), None);
        assert!(spans.iter().any(|s| s.text.contains("[a ")), "{spans:?}");
    }

    #[test]
    fn only_http_and_https_can_be_opened() {
        assert!(openable("https://github.com/x"));
        assert!(openable("HTTP://example.com"));
        for bad in [
            "file:///C:/x.exe",
            "C:\\Windows\\System32\\calc.exe",
            "\\\\server\\share\\x.exe",
            "javascript:alert(1)",
            "ms-settings:",
            "mailto:a@b.c",
            "https://",
            "https://a b",
            "https://a\nb",
            "",
        ] {
            assert!(!openable(bad), "{bad}");
        }
    }

    #[test]
    fn one_quote_level_and_rules() {
        let blocks = parse("> quoted\n> > deeper\n\n---\n");
        assert_eq!(
            blocks,
            vec![
                Block {
                    kind: Kind::Paragraph,
                    spans: vec![text("quoted > deeper")],
                    quote: true
                },
                block(Kind::Rule, vec![]),
            ]
        );
    }

    #[test]
    fn headings_need_a_space_and_lose_closing_hashes() {
        assert_eq!(
            parse("## Fixes ##\n#hashtag\n####### seven"),
            vec![
                block(Kind::Heading(2), vec![text("Fixes")]),
                para("#hashtag ####### seven"),
            ]
        );
    }

    #[test]
    fn a_line_break_stays_a_line_break() {
        assert_eq!(parse("a  \nb"), vec![para("a\nb")]);
    }

    #[test]
    fn empty_and_huge_notes() {
        assert_eq!(parse(""), Vec::<Block>::new());
        let huge = "x".repeat(NOTES_LIMIT + 10);
        let blocks = parse(&huge);
        assert_eq!(blocks[0].spans[0].text.chars().count(), NOTES_LIMIT);
    }

    /// Inputs that make recursive or backtracking parsers deep or slow,
    /// each at the size limit, on a thread with a 1 MB stack.
    #[test]
    fn hostile_notes_parse_fast_on_a_small_stack() {
        let half = NOTES_LIMIT / 2;
        let inputs: Vec<(&str, String)> = vec![
            (
                "emphasis",
                format!("{}x{}", "*".repeat(half), "*".repeat(half - 1)),
            ),
            ("quotes", ">".repeat(NOTES_LIMIT)),
            (
                "nested items",
                (0..2000)
                    .map(|d| format!("{}- x\n", " ".repeat(d)))
                    .collect(),
            ),
            ("brackets", "[".repeat(NOTES_LIMIT)),
            ("brackets closed", "[](".repeat(NOTES_LIMIT / 3)),
            ("mixed", "[*`_~<!(".repeat(NOTES_LIMIT / 8)),
            (
                "backtick lengths",
                (1..400).map(|k| "`".repeat(k) + "a").collect(),
            ),
            ("underscores", "_a".repeat(half)),
            ("fences", "```\n".repeat(NOTES_LIMIT / 4)),
        ];
        for (name, input) in inputs {
            let worker = std::thread::Builder::new()
                .stack_size(1 << 20)
                .spawn(move || {
                    let started = Instant::now();
                    let blocks = parse(&input);
                    drop(blocks);
                    started.elapsed()
                })
                .unwrap();
            let took = worker.join().unwrap_or_else(|_| panic!("{name} panicked"));
            assert!(took < Duration::from_secs(2), "{name} took {took:?}");
        }
    }
}
