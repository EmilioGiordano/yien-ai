//! A release's notes (GitHub markdown) as blocks the window can draw:
//! headings, paragraphs, lists, quotes, code and links. Raw HTML is shown as
//! its text, an image as its alt text, and nothing is ever loaded from the
//! network; a link is only a target the user may click.

use std::collections::HashMap;

use markdown::mdast::Node;

/// The most characters of notes read; the rest is cut.
const NOTES_LIMIT: usize = 64 * 1024;

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

/// One block of the notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    /// `start` is the first number of an ordered list, `None` for bullets.
    List {
        start: Option<u32>,
        items: Vec<Vec<Block>>,
    },
    Code(String),
    Quote(Vec<Block>),
    Rule,
}

/// Whether a link target may be opened in the browser: `http` or `https`
/// only, so a click can never run a local file or another URL scheme.
pub(crate) fn openable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && lower.len() > "https://".len()
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// The notes as blocks. Text the parser cannot read is one paragraph.
pub(crate) fn parse(notes: &str) -> Vec<Block> {
    let notes = cut(notes);
    match markdown::to_mdast(&notes, &markdown::ParseOptions::gfm()) {
        Ok(root) => {
            let mut links = HashMap::new();
            definitions(&root, &mut links);
            let reader = Reader { links };
            reader.blocks(root.children().map(Vec::as_slice).unwrap_or_default())
        }
        Err(_) => vec![Block::Paragraph(vec![Span::plain(notes)])],
    }
}

fn cut(notes: &str) -> String {
    match notes.char_indices().nth(NOTES_LIMIT) {
        Some((at, _)) => format!("{}\n\n…", &notes[..at]),
        None => notes.to_owned(),
    }
}

/// Link reference definitions (`[id]: url`), by identifier.
fn definitions(node: &Node, out: &mut HashMap<String, String>) {
    if let Node::Definition(d) = node {
        out.entry(d.identifier.to_lowercase())
            .or_insert_with(|| d.url.clone());
    }
    for child in node.children().into_iter().flatten() {
        definitions(child, out);
    }
}

struct Reader {
    links: HashMap<String, String>,
}

impl Reader {
    fn blocks(&self, nodes: &[Node]) -> Vec<Block> {
        let mut out = Vec::new();
        for node in nodes {
            self.block(node, &mut out);
        }
        out
    }

    fn block(&self, node: &Node, out: &mut Vec<Block>) {
        match node {
            Node::Heading(h) => out.push(Block::Heading {
                level: h.depth.clamp(1, 6),
                spans: self.spans(&h.children),
            }),
            Node::Paragraph(p) => {
                let spans = self.spans(&p.children);
                if !spans.is_empty() {
                    out.push(Block::Paragraph(spans));
                }
            }
            Node::List(list) => out.push(Block::List {
                start: list.ordered.then_some(list.start.unwrap_or(1)),
                items: list.children.iter().map(|item| self.item(item)).collect(),
            }),
            Node::Code(code) => out.push(Block::Code(code.value.clone())),
            Node::Math(math) => out.push(Block::Code(math.value.clone())),
            Node::Html(html) => out.push(Block::Paragraph(vec![Span::plain(html.value.clone())])),
            Node::Blockquote(q) => out.push(Block::Quote(self.blocks(&q.children))),
            Node::ThematicBreak(_) => out.push(Block::Rule),
            Node::Table(table) => {
                for row in &table.children {
                    let mut spans = Vec::new();
                    for (n, cell) in row.children().into_iter().flatten().enumerate() {
                        if n > 0 {
                            push(&mut spans, Span::plain(" | "));
                        }
                        for span in
                            self.spans(cell.children().map(Vec::as_slice).unwrap_or_default())
                        {
                            push(&mut spans, span);
                        }
                    }
                    if !spans.is_empty() {
                        out.push(Block::Paragraph(spans));
                    }
                }
            }
            Node::FootnoteDefinition(f) => {
                out.push(Block::Paragraph(vec![Span::plain(format!(
                    "[^{}]",
                    f.label.as_deref().unwrap_or(&f.identifier)
                ))]));
                out.extend(self.blocks(&f.children));
            }
            Node::Definition(_) | Node::Yaml(_) | Node::Toml(_) => {}
            other => {
                // Inline content where a block was expected.
                let spans = self.spans(std::slice::from_ref(other));
                if !spans.is_empty() {
                    out.push(Block::Paragraph(spans));
                }
            }
        }
    }

    fn item(&self, item: &Node) -> Vec<Block> {
        let Node::ListItem(item) = item else {
            return self.blocks(std::slice::from_ref(item));
        };
        let mut blocks = self.blocks(&item.children);
        if let Some(checked) = item.checked {
            let mark = Span::plain(if checked { "[x] " } else { "[ ] " });
            match blocks.first_mut() {
                Some(Block::Paragraph(spans)) => {
                    let rest = std::mem::take(spans);
                    push(spans, mark);
                    for span in rest {
                        push(spans, span);
                    }
                }
                _ => blocks.insert(0, Block::Paragraph(vec![mark])),
            }
        }
        blocks
    }

    fn spans(&self, nodes: &[Node]) -> Vec<Span> {
        let mut out = Vec::new();
        for node in nodes {
            self.inline(node, &Span::default(), &mut out);
        }
        out
    }

    fn inline(&self, node: &Node, style: &Span, out: &mut Vec<Span>) {
        let text = |text: &str| Span {
            text: text.to_owned(),
            ..style.clone()
        };
        let children = |style: Span, out: &mut Vec<Span>| {
            for child in node.children().into_iter().flatten() {
                self.inline(child, &style, out);
            }
        };
        match node {
            Node::Text(t) => push(out, text(&t.value)),
            Node::Html(h) => push(out, text(&h.value)),
            Node::Break(_) => push(out, text("\n")),
            Node::InlineCode(c) => push(
                out,
                Span {
                    code: true,
                    ..text(&c.value)
                },
            ),
            Node::InlineMath(m) => push(
                out,
                Span {
                    code: true,
                    ..text(&m.value)
                },
            ),
            Node::Image(i) => push(out, text(&i.alt)),
            Node::ImageReference(i) => push(out, text(&i.alt)),
            Node::FootnoteReference(f) => push(
                out,
                text(&format!(
                    "[^{}]",
                    f.label.as_deref().unwrap_or(&f.identifier)
                )),
            ),
            Node::Emphasis(_) => children(
                Span {
                    italic: true,
                    ..style.clone()
                },
                out,
            ),
            Node::Strong(_) => children(
                Span {
                    bold: true,
                    ..style.clone()
                },
                out,
            ),
            Node::Delete(_) => children(
                Span {
                    strike: true,
                    ..style.clone()
                },
                out,
            ),
            Node::Link(link) => children(
                Span {
                    link: openable(&link.url).then(|| link.url.trim().to_owned()),
                    ..style.clone()
                },
                out,
            ),
            Node::LinkReference(r) => {
                let url = self.links.get(&r.identifier.to_lowercase());
                children(
                    Span {
                        link: url.filter(|u| openable(u)).map(|u| u.trim().to_owned()),
                        ..style.clone()
                    },
                    out,
                );
            }
            _ => children(style.clone(), out),
        }
    }
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
    use super::*;

    fn text(s: &str) -> Span {
        Span::plain(s)
    }

    fn para(text: &str) -> Block {
        Block::Paragraph(vec![Span::plain(text)])
    }

    #[test]
    fn headings_paragraphs_and_inline_styles() {
        let blocks =
            parse("# YIEN 0.1.0\n\nThe **first** *pre-release*, with `code` and ~~gone~~.\n");
        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 1,
                    spans: vec![text("YIEN 0.1.0")]
                },
                Block::Paragraph(vec![
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
                ]),
            ]
        );
    }

    #[test]
    fn nested_bold_italic_keeps_both() {
        let blocks = parse("***both***");
        assert_eq!(
            blocks,
            vec![Block::Paragraph(vec![Span {
                bold: true,
                italic: true,
                ..text("both")
            }])]
        );
    }

    #[test]
    fn lists_ordered_bulleted_nested_and_tasks() {
        let blocks = parse(
            "- one\n- two\n  1. inner\n  2. more\n\n3. three\n4. four\n\n- [x] done\n- [ ] todo\n",
        );
        assert_eq!(
            blocks,
            vec![
                Block::List {
                    start: None,
                    items: vec![
                        vec![para("one")],
                        vec![
                            para("two"),
                            Block::List {
                                start: Some(1),
                                items: vec![vec![para("inner")], vec![para("more")]]
                            }
                        ],
                    ]
                },
                Block::List {
                    start: Some(3),
                    items: vec![vec![para("three")], vec![para("four")]]
                },
                Block::List {
                    start: None,
                    items: vec![
                        vec![Block::Paragraph(vec![text("[x] done")])],
                        vec![Block::Paragraph(vec![text("[ ] todo")])],
                    ]
                },
            ]
        );
    }

    #[test]
    fn code_blocks_keep_their_text_verbatim() {
        let blocks =
            parse("```powershell\nGet-FileHash .\\yien.exe -Algorithm SHA256\n<b>x</b>\n```\n");
        assert_eq!(
            blocks,
            vec![Block::Code(
                "Get-FileHash .\\yien.exe -Algorithm SHA256\n<b>x</b>".into()
            )]
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
        let blocks = parse(
            "![the new tab bar](https://example.com/shot.png)\n\n![][logo]\n\n[logo]: https://example.com/logo.png\n",
        );
        assert_eq!(blocks, vec![para("the new tab bar")]);
        let all = format!("{blocks:?}");
        assert!(!all.contains("example.com"), "{all}");
    }

    #[test]
    fn links_keep_only_web_targets() {
        let blocks = parse(
            "[page](https://github.com/EmilioGiordano/yien-ai) [local](file:///C:/Windows/System32/calc.exe) \
             [js](javascript:alert(1)) [ref][r] <https://example.com/auto>\n\n[r]: http://example.com/ref\n",
        );
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("{blocks:?}");
        };
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
        assert_eq!(link("ref").as_deref(), Some("http://example.com/ref"));
        assert_eq!(
            link("https://example.com/auto").as_deref(),
            Some("https://example.com/auto")
        );
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
    fn quotes_rules_and_tables_fall_back_to_text() {
        let blocks = parse("> quoted\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(
            blocks,
            vec![
                Block::Quote(vec![para("quoted")]),
                Block::Rule,
                para("a | b"),
                para("1 | 2"),
            ]
        );
    }

    #[test]
    fn empty_and_huge_notes() {
        assert_eq!(parse(""), Vec::<Block>::new());
        let huge = "x".repeat(NOTES_LIMIT + 10);
        let Block::Paragraph(spans) = &parse(&huge)[0] else {
            panic!();
        };
        assert_eq!(spans[0].text.chars().count(), NOTES_LIMIT);
    }

    #[test]
    fn a_line_break_stays_a_line_break() {
        assert_eq!(parse("a  \nb"), vec![para("a\nb")]);
    }
}
