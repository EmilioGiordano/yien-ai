//! Draws the rendered rows of a Markdown file in a file preview pane, and
//! the Rendered/Source toggle in its header (RF-CTX-02). The rows come
//! parsed from the core; this module only draws them.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::h_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::PaneId;
use yien_proto::file_preview::{
    FileLine, MdAlign, MdBlock, MdColumn, MdRow, MdStyle, MdText, PreviewMode, marker_label,
};

use crate::theme::Theme;

/// The rendered body's padding left and right of the rows.
pub(crate) const BODY_PAD_X: f32 = 18.0;
/// The rendered body's padding above the first row.
pub(crate) const BODY_PAD_TOP: f32 = 12.0;
/// The rendered body's padding below the last row.
pub(crate) const BODY_PAD_BOTTOM: f32 = 12.0;
/// How far each list level moves its rows in.
const INDENT_W: f32 = 20.0;
/// Quote bars drawn at most; deeper quotes share the last one.
const MAX_QUOTE_BARS: u8 = 8;
/// List levels drawn at most; deeper ones line up with the last.
const MAX_INDENT: u8 = 12;
/// A column's width per character, as a share of the table text size.
const CHAR_W: f32 = 0.56;
/// A table cell's padding left and right.
const CELL_PAD_X: f32 = 10.0;

/// What every rendered row is drawn with.
#[derive(Clone)]
pub(crate) struct Look {
    pub(crate) theme: Theme,
    /// The code font family.
    pub(crate) mono: SharedString,
    pub(crate) code_size: f32,
    pub(crate) code_line_h: f32,
    pub(crate) wrap: bool,
}

/// Where each drawn row's top and bottom edges fell at the last paint, by
/// its index in the frame.
pub(crate) type RowSpans = Rc<RefCell<Vec<(f32, f32)>>>;

/// The height of a line of rendered prose, the shortest row but a code line.
pub(crate) fn prose_line_h(t: &Theme) -> f32 {
    t.text(13.0).as_f32() * 1.55
}

/// The heights of the first `len` rows, or `None` while one is unmeasured.
pub(crate) fn heights(spans: &[(f32, f32)], len: usize) -> Option<Vec<f32>> {
    let spans = spans.get(..len)?;
    spans
        .iter()
        .map(|(top, bottom)| {
            let h = bottom - top;
            (h.is_finite() && h >= 0.0).then_some(h)
        })
        .collect()
}

/// A scroll position over rendered rows: the row the view's top edge falls
/// in, and how many pixels of that row are above the edge.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct ScrollPos {
    pub(crate) row: usize,
    pub(crate) offset: f32,
}

fn tops(heights: &[f32]) -> Vec<f32> {
    let mut y = 0.0;
    let mut tops = Vec::with_capacity(heights.len() + 1);
    tops.push(0.0);
    for h in heights {
        y += h;
        tops.push(y);
    }
    tops
}

/// The position `by` pixels from `from` over rows of `heights` seen through
/// a view `avail` pixels tall, kept between the first row's top and the
/// point where the last row's bottom meets the view's bottom. `None` when
/// `from` is not one of the rows.
pub(crate) fn scroll_rows(
    heights: &[f32],
    from: ScrollPos,
    by: f32,
    avail: f32,
) -> Option<ScrollPos> {
    if from.row >= heights.len() || by.is_nan() {
        return None;
    }
    let tops = tops(heights);
    let end = (tops[heights.len()] - avail.max(0.0)).max(0.0);
    let at = (tops[from.row] + from.offset.max(0.0) + by).clamp(0.0, end);
    let row = tops[..heights.len()]
        .partition_point(|top| *top <= at)
        .saturating_sub(1);
    Some(ScrollPos {
        row,
        offset: at - tops[row],
    })
}

/// The rows from `from` that are at least partly in the view.
pub(crate) fn shown_rows(heights: &[f32], from: ScrollPos, avail: f32) -> usize {
    let tops = tops(heights);
    let Some(start) = tops.get(from.row) else {
        return 0;
    };
    let at = start + from.offset;
    (from.row..heights.len())
        .take_while(|&k| tops[k] < at + avail)
        .count()
}

/// The rows from `from` that are wholly in the view: the page, for the
/// scrollbar.
pub(crate) fn whole_rows(heights: &[f32], from: ScrollPos, avail: f32) -> usize {
    let tops = tops(heights);
    let Some(start) = tops.get(from.row) else {
        return 0;
    };
    let at = start + from.offset;
    (from.row..heights.len())
        .filter(|&k| tops[k] + 0.5 >= at && tops[k + 1] <= at + avail + 0.5)
        .count()
}

/// The rows to ask the core for below the anchor: enough of the shortest
/// row to fill `avail`, and one more.
pub(crate) fn rows_for(avail: f32, shortest: f32) -> usize {
    if !avail.is_finite() || !shortest.is_finite() || shortest <= 0.0 || avail <= 0.0 {
        return 1;
    }
    (avail / shortest).ceil() as usize + 1
}

/// A heading's text size and the space above and below it, at 13 px.
pub(crate) fn heading_metrics(level: u8) -> (f32, f32, f32) {
    match level {
        1 => (20.0, 0.0, 10.0),
        2 => (15.0, 16.0, 6.0),
        3 => (13.5, 12.0, 4.0),
        _ => (13.0, 10.0, 4.0),
    }
}

/// The space below a row that ends its block, at 13 px.
pub(crate) fn space_after(block: &MdBlock) -> f32 {
    match block {
        MdBlock::Heading { .. } => 0.0,
        MdBlock::Item { .. } | MdBlock::TableRow { .. } => 10.0,
        MdBlock::Paragraph { .. } | MdBlock::Code { .. } | MdBlock::Rule => 8.0,
    }
}

/// The highlight for each styled range of `text`; ranges that do not fit
/// the text are dropped.
pub(crate) fn inline_highlights(text: &MdText, t: &Theme) -> Vec<(Range<usize>, HighlightStyle)> {
    text.spans
        .iter()
        .filter(|s| fits(&text.text, s.start, s.end))
        .map(|s| (s.start..s.end, style_of(s.style, t)))
        .collect()
}

/// The ranges of `text` drawn in the code font.
pub(crate) fn code_ranges(text: &MdText) -> Vec<Range<usize>> {
    text.spans
        .iter()
        .filter(|s| s.style.code && fits(&text.text, s.start, s.end))
        .map(|s| s.start..s.end)
        .collect()
}

fn fits(text: &str, start: usize, end: usize) -> bool {
    start < end && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end)
}

fn style_of(style: MdStyle, t: &Theme) -> HighlightStyle {
    let color = if style.link {
        Some(t.ai)
    } else if style.image {
        Some(t.term_muted)
    } else {
        None
    };
    HighlightStyle {
        color: color.map(Into::into),
        font_weight: style.strong.then_some(FontWeight::BOLD),
        font_style: (style.emphasis || style.image).then_some(FontStyle::Italic),
        background_color: style.code.then(|| t.term_raised.into()),
        underline: style.link.then(|| UnderlineStyle {
            thickness: px(1.0),
            color: Some(Rgba { a: 0.45, ..t.ai }.into()),
            wavy: false,
        }),
        strikethrough: style.strike.then(|| StrikethroughStyle {
            thickness: px(1.0),
            color: Some(t.term_muted.into()),
        }),
        ..HighlightStyle::default()
    }
}

/// A table column's width for `column.width` characters at `size` pixels.
pub(crate) fn column_width(column: MdColumn, size: f32) -> f32 {
    (f32::from(column.width.max(1)) * size * CHAR_W).ceil() + 2.0 * CELL_PAD_X
}

fn inline_text(text: &MdText, look: &Look) -> AnyElement {
    let shown = if text.text.is_empty() {
        SharedString::new_static(" ")
    } else {
        SharedString::from(text.text.clone())
    };
    let highlights = inline_highlights(text, &look.theme);
    let mono = code_ranges(text);
    if highlights.is_empty() {
        return StyledText::new(shown).into_any_element();
    }
    let family = look.mono.clone();
    StyledText::new(shown)
        .with_highlights(highlights)
        .with_font_family_overrides(mono.into_iter().map(move |r| (r, family.clone())))
        .into_any_element()
}

fn code_text(line: &FileLine, look: &Look) -> AnyElement {
    let t = &look.theme;
    let shown = if line.text.is_empty() {
        SharedString::new_static(" ")
    } else {
        SharedString::from(line.text.clone())
    };
    let highlights: Vec<(Range<usize>, HighlightStyle)> = line
        .spans
        .iter()
        .filter(|s| fits(&line.text, s.start, s.end))
        .map(|s| {
            let style = HighlightStyle {
                color: Some(t.syntax_color(s.kind).into()),
                ..HighlightStyle::default()
            };
            (s.start..s.end, style)
        })
        .collect();
    if highlights.is_empty() {
        StyledText::new(shown).into_any_element()
    } else {
        StyledText::new(shown)
            .with_highlights(highlights)
            .into_any_element()
    }
}

fn block_body(row: &MdRow, look: &Look, top_of_doc: bool) -> AnyElement {
    let t = &look.theme;
    match &row.block {
        MdBlock::Heading { level, text } => {
            let (size, above, below) = heading_metrics(*level);
            div()
                .when(!top_of_doc && row.first, |d| d.pt(px(above)))
                .when(row.last, |d| d.pb(px(below)))
                .child(
                    div()
                        .text_size(t.text(size))
                        .line_height(relative(1.3))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.term_text)
                        .when(*level == 1 && row.last, |d| {
                            d.pb(px(6.0)).border_b_1().border_color(t.term_line)
                        })
                        .child(inline_text(text, look)),
                )
                .into_any_element()
        }
        MdBlock::Paragraph { text } => div().child(inline_text(text, look)).into_any_element(),
        MdBlock::Item { marker, text } => h_flex()
            .items_start()
            .child(
                div()
                    .flex_none()
                    .w(px(INDENT_W))
                    .text_color(t.term_faint)
                    .when_some(
                        matches!(marker, yien_proto::file_preview::MdMarker::Task { .. })
                            .then(|| look.mono.clone()),
                        |d, family| d.font_family(family).text_size(px(look.code_size)),
                    )
                    .child(marker_label(*marker)),
            )
            .child(div().flex_1().min_w_0().child(inline_text(text, look)))
            .into_any_element(),
        MdBlock::Code { line } => div()
            .px(px(10.0))
            .when(row.first, |d| d.pt(px(8.0)).border_t_1().rounded_t(px(6.0)))
            .when(row.last, |d| d.pb(px(8.0)).border_b_1().rounded_b(px(6.0)))
            .border_x_1()
            .border_color(t.term_line)
            .bg(t.term_raised)
            .font_family(look.mono.clone())
            .text_size(px(look.code_size))
            .line_height(px(look.code_line_h))
            .text_color(t.term_text)
            .when(!look.wrap, |d| d.whitespace_nowrap().overflow_x_hidden())
            .child(code_text(line, look))
            .into_any_element(),
        MdBlock::TableRow {
            cells,
            columns,
            header,
        } => {
            let size = 12.5 * t.ui_size / crate::theme::BASE_UI_SIZE;
            let cells: Vec<AnyElement> = cells
                .iter()
                .zip(columns)
                .enumerate()
                .map(|(idx, (cell, column))| {
                    div()
                        .flex_basis(px(column_width(*column, size)))
                        .when(look.wrap, |d| d.flex_shrink(1.0))
                        .when(!look.wrap, |d| d.flex_none().whitespace_nowrap())
                        .min_w_0()
                        .overflow_hidden()
                        .px(px(CELL_PAD_X))
                        .py(px(4.0))
                        .border_r_1()
                        .border_b_1()
                        .when(idx == 0, |d| d.border_l_1())
                        .when(row.first, |d| d.border_t_1())
                        .border_color(t.term_line)
                        .when(*header, |d| {
                            d.bg(t.term_raised).font_weight(FontWeight::MEDIUM)
                        })
                        .map(|d| match column.align {
                            MdAlign::Center => d.text_center(),
                            MdAlign::Right => d.text_right(),
                            MdAlign::None | MdAlign::Left => d,
                        })
                        .child(inline_text(cell, look))
                        .into_any_element()
                })
                .collect();
            h_flex()
                .items_stretch()
                .max_w_full()
                .overflow_hidden()
                .text_size(t.text(12.5))
                .children(cells)
                .into_any_element()
        }
        MdBlock::Rule => div()
            .h(px(look.code_line_h))
            .flex()
            .items_center()
            .child(div().w_full().h(px(1.0)).bg(t.term_line))
            .into_any_element(),
    }
}

/// One rendered row: its block, moved in by its list level, inside its
/// quote bars, with the space that follows it. `spans` learns where it
/// lies at each paint, under its `index` in the frame.
pub(crate) fn render_row(
    row: &MdRow,
    index: usize,
    top_of_doc: bool,
    look: &Look,
    spans: &RowSpans,
) -> AnyElement {
    let t = &look.theme;
    let indent = row.indent.min(MAX_INDENT);
    let indent = match row.block {
        MdBlock::Item { .. } => indent.saturating_sub(1),
        _ => indent,
    };
    let gap = if row.last {
        space_after(&row.block)
    } else {
        0.0
    };
    let mut el = div()
        .pl(px(f32::from(indent) * INDENT_W))
        .pb(px(gap))
        .child(block_body(row, look, top_of_doc))
        .into_any_element();
    for _ in 0..row.quote.min(MAX_QUOTE_BARS) {
        el = div()
            .pl(px(10.0))
            .border_l_2()
            .border_color(t.term_strong)
            .text_color(t.term_muted)
            .child(el)
            .into_any_element();
    }
    let spans = Rc::clone(spans);
    div()
        .relative()
        .w_full()
        .child(el)
        .child(
            canvas(
                move |bounds, _, _| {
                    let mut spans = spans.borrow_mut();
                    if spans.len() <= index {
                        spans.resize(index + 1, (f32::NAN, f32::NAN));
                    }
                    let top = bounds.origin.y.as_f32();
                    spans[index] = (top, top + bounds.size.height.as_f32());
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .into_any_element()
}

/// The Rendered/Source segmented toggle a Markdown file's header shows next
/// to Wrap.
pub(crate) fn mode_toggle(
    pane: PaneId,
    mode: PreviewMode,
    theme: &Theme,
    mono: Option<SharedString>,
    on_pick: impl Fn(PreviewMode, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let t = *theme;
    let on_pick = Rc::new(on_pick);
    let button = |which: PreviewMode, label: &'static str, first: bool| {
        let on = mode == which;
        let pick = Rc::clone(&on_pick);
        div()
            .id((label, pane.get()))
            .role(Role::Button)
            .aria_label(format!("Show {} Markdown", label.to_lowercase()))
            .aria_toggled(if on { Toggled::True } else { Toggled::False })
            .flex()
            .items_center()
            .px(px(6.0))
            .when(!first, |d| d.border_l_1().border_color(t.term_line))
            .when(on, |d| d.bg(t.term_bg))
            .text_color(if on { t.term_text } else { t.term_muted })
            .cursor_pointer()
            .when(!on, |d| d.hover(move |s| s.bg(t.term_raised)))
            .on_click(move |_, window, cx| pick(which, window, cx))
            .child(label)
    };
    h_flex()
        .id(("file-view-mode", pane.get()))
        .occlude()
        .role(Role::Group)
        .aria_label("View")
        .flex_none()
        .items_stretch()
        .h(px(20.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(t.term_line)
        .overflow_hidden()
        .text_size(t.text(11.0))
        .when_some(mono, |d, family| d.font_family(family))
        .child(button(PreviewMode::Rendered, "Rendered", true))
        .child(button(PreviewMode::Source, "Source", false))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the gpui glob there would shadow `#[test]`.
    use gpui_kit::{FontStyle, FontWeight};
    use yien_proto::file_preview::{MdAlign, MdBlock, MdColumn, MdSpan, MdStyle, MdText};

    use std::ops::Range;

    use super::{
        ScrollPos, code_ranges, column_width, heading_metrics, heights, inline_highlights,
        rows_for, scroll_rows, shown_rows, space_after, whole_rows,
    };
    use crate::theme::{Theme, ThemeKind};

    fn text(s: &str, spans: Vec<(usize, usize, MdStyle)>) -> MdText {
        MdText {
            text: s.into(),
            spans: spans
                .into_iter()
                .map(|(start, end, style)| MdSpan { start, end, style })
                .collect(),
        }
    }

    fn pos(row: usize, offset: f32) -> ScrollPos {
        ScrollPos { row, offset }
    }

    #[test]
    fn rf_ctx_02_markdown_a_row_taller_than_the_pane_scrolls_through_by_pixels() {
        // A 580 px paragraph between two short rows, seen through 350 px.
        let h = [20.0, 580.0, 20.0];
        let mut at = pos(0, 0.0);
        let mut seen = Vec::new();
        for _ in 0..40 {
            at = scroll_rows(&h, at, 20.0, 350.0).unwrap();
            seen.push(at);
        }
        // It stops where the last row's bottom meets the view's bottom.
        assert_eq!(at, pos(1, 620.0 - 350.0 - 20.0));
        // Every pixel of the tall row was reachable: its bottom 230 px came
        // into view on the way.
        assert!(seen.iter().any(|p| p.row == 1 && p.offset + 350.0 >= 580.0));
        // And back up, by pixels, to the very top.
        for _ in 0..40 {
            at = scroll_rows(&h, at, -20.0, 350.0).unwrap();
        }
        assert_eq!(at, pos(0, 0.0));
    }

    #[test]
    fn rf_ctx_02_markdown_page_keys_move_by_the_view_over_rows_of_any_height() {
        // Rows 500 px tall in all, seen through 200 px.
        let h = [20.0, 20.0, 100.0, 20.0, 300.0, 20.0, 20.0];
        // PageDown by a page less a line: 180 px.
        let at = scroll_rows(&h, pos(0, 0.0), 180.0, 200.0).unwrap();
        assert_eq!(at, pos(4, 20.0));
        // End: the last row's bottom at the view's bottom, from anywhere.
        let end = pos(4, 140.0);
        assert_eq!(scroll_rows(&h, pos(0, 0.0), f32::MAX, 200.0).unwrap(), end);
        assert_eq!(scroll_rows(&h, pos(6, 0.0), f32::MAX, 200.0).unwrap(), end);
        // A second PageDown stops at the end; Home goes back to the top.
        assert_eq!(scroll_rows(&h, at, 180.0, 200.0).unwrap(), end);
        assert_eq!(scroll_rows(&h, end, 180.0, 200.0).unwrap(), end);
        assert_eq!(scroll_rows(&h, end, -f32::MAX, 200.0).unwrap(), pos(0, 0.0));
        // Rows shorter than the view never scroll.
        assert_eq!(
            scroll_rows(&[20.0, 30.0], pos(0, 0.0), 100.0, 300.0).unwrap(),
            pos(0, 0.0)
        );
        // A row that is not in the frame gives no position.
        assert_eq!(scroll_rows(&h, pos(7, 0.0), 1.0, 300.0), None);
        assert_eq!(scroll_rows(&[], pos(0, 0.0), 1.0, 300.0), None);
    }

    #[test]
    fn rf_ctx_02_markdown_the_scrollbar_page_counts_whole_rows_in_view() {
        let h = [20.0, 20.0, 100.0, 20.0, 300.0];
        assert_eq!(whole_rows(&h, pos(0, 0.0), 150.0), 3);
        assert_eq!(shown_rows(&h, pos(0, 0.0), 150.0), 4);
        // Ten pixels into the first row: it no longer counts as whole.
        assert_eq!(whole_rows(&h, pos(0, 10.0), 150.0), 3);
        assert_eq!(whole_rows(&h, pos(0, 10.0), 140.0), 2);
        // Inside a row taller than the view, nothing is whole.
        assert_eq!(whole_rows(&h, pos(4, 50.0), 150.0), 0);
        assert_eq!(shown_rows(&h, pos(4, 50.0), 150.0), 1);
        assert_eq!(whole_rows(&h, pos(9, 0.0), 150.0), 0);
    }

    #[test]
    fn rf_ctx_02_markdown_heights_wait_for_every_row_to_be_measured() {
        let spans = [(10.0, 30.0), (30.0, 90.0), (f32::NAN, f32::NAN)];
        assert_eq!(heights(&spans, 2), Some(vec![20.0, 60.0]));
        assert_eq!(heights(&spans, 3), None);
        assert_eq!(heights(&spans, 4), None);
        assert_eq!(heights(&spans, 0), Some(Vec::new()));
    }

    #[test]
    fn rf_ctx_02_markdown_the_core_is_asked_for_rows_by_the_shortest_row() {
        // A 600 px view of 20 px prose rows needs 31, whatever the code font.
        assert_eq!(rows_for(600.0, 20.0), 31);
        assert_eq!(rows_for(600.0, 20.0_f32.min(33.0)), 31);
        assert_eq!(rows_for(0.0, 20.0), 1);
        assert_eq!(rows_for(600.0, 0.0), 1);
        assert_eq!(rows_for(f32::NAN, 20.0), 1);
    }

    #[test]
    fn rf_ctx_02_markdown_styles_map_to_highlights_and_never_red() {
        for kind in [ThemeKind::Sumi, ThemeKind::Washi] {
            let t = Theme::of(kind);
            let link = MdStyle {
                link: true,
                ..MdStyle::default()
            };
            let code = MdStyle {
                code: true,
                ..MdStyle::default()
            };
            let both = MdStyle {
                strong: true,
                emphasis: true,
                strike: true,
                ..MdStyle::default()
            };
            let md = text(
                "link code all",
                vec![(0, 4, link), (5, 9, code), (10, 13, both)],
            );
            let hl = inline_highlights(&md, &t);
            assert_eq!(hl.len(), 3);
            assert_eq!(hl[0].1.color, Some(t.ai.into()));
            assert!(hl[0].1.underline.is_some());
            assert_eq!(hl[1].1.background_color, Some(t.term_raised.into()));
            assert_eq!(hl[2].1.font_weight, Some(FontWeight::BOLD));
            assert_eq!(hl[2].1.font_style, Some(FontStyle::Italic));
            assert!(hl[2].1.strikethrough.is_some());
            for (_, style) in &hl {
                assert_ne!(style.color, Some(t.shu.into()));
                assert_ne!(style.color, Some(t.mark_red.into()));
            }
            assert_eq!(code_ranges(&md), vec![Range { start: 5, end: 9 }]);
        }
    }

    #[test]
    fn rf_ctx_02_markdown_spans_outside_the_text_are_dropped() {
        let style = MdStyle {
            strong: true,
            ..MdStyle::default()
        };
        let md = text(
            "é b",
            vec![(1, 2, style), (0, 9, style), (3, 3, style), (3, 4, style)],
        );
        let hl = inline_highlights(&md, &Theme::of(ThemeKind::Sumi));
        let ranges: Vec<(usize, usize)> = hl.iter().map(|(r, _)| (r.start, r.end)).collect();
        assert_eq!(ranges, [(3, 4)]);
    }

    #[test]
    fn rf_ctx_02_markdown_columns_grow_with_their_widest_cell() {
        let col = |width| MdColumn {
            width,
            align: MdAlign::None,
        };
        assert_eq!(column_width(col(10), 10.0), 76.0);
        assert_eq!(column_width(col(0), 10.0), column_width(col(1), 10.0));
        assert!(column_width(col(20), 12.5) > column_width(col(10), 12.5));
    }

    #[test]
    fn rf_ctx_02_markdown_headings_shrink_by_level_and_blocks_leave_space() {
        let sizes: Vec<f32> = (1..=6).map(|l| heading_metrics(l).0).collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]));
        assert_eq!(heading_metrics(1).1, 0.0);
        assert_eq!(space_after(&MdBlock::Rule), 8.0);
    }
}
