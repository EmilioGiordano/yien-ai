//! The read-only file preview pane body (RF-CTX-02, ADR-06): draws only the
//! visible line slice from [`FileFrame`], with line numbers, syntax highlights,
//! soft wrap toggle, and scrolling by wheel, keys and scrollbar. A Markdown
//! file shown rendered draws its visible rows instead.

use std::ops::Range;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::file_preview::{FileFrame, FileLine, FileNotice, MdRow};
use yien_proto::test_control::FileScroll;
use yien_proto::{ClientMsg, Command, PaneId};

use crate::bridge::{Link, Sent, deliver};
use crate::file_markdown::{
    self, BODY_PAD_BOTTOM, BODY_PAD_TOP, BODY_PAD_X, Look, RowSpans, ScrollPos, heights,
    prose_line_h, rows_for, scroll_rows, shown_rows, whole_rows,
};
use crate::test_control::TerminalFacts;
use crate::theme::Theme;

const FALLBACK_MONO: &str = "Consolas";
/// The code area's padding above and below the lines.
const CODE_PAD_Y: f32 = 4.0;
/// The gutter's space right of its numbers; its 1 px border comes on top.
const GUTTER_PAD_RIGHT: f32 = 9.0;
/// The gutter never narrows below this many digits.
const MIN_GUTTER_DIGITS: usize = 3;

/// The gutter's outer width for a file of `total_lines`: its widest number
/// at `digit_w` (the font's measured advance of "0"; 0.6 of `font_size`
/// when unknown), plus its padding and border.
pub(crate) fn gutter_width(total_lines: usize, digit_w: Option<f32>, font_size: f32) -> f32 {
    let digits = total_lines
        .checked_ilog10()
        .map_or(1, |log| log as usize + 1)
        .max(MIN_GUTTER_DIGITS);
    let digit_w = digit_w
        .filter(|w| w.is_finite() && *w > 0.0)
        .unwrap_or(font_size * 0.6);
    (digits as f32 * digit_w).ceil() + GUTTER_PAD_RIGHT + 1.0
}

/// The whole lines that fit in a code area `height` pixels tall; at least one.
pub(crate) fn rows_in(height: f32, line_h: f32) -> usize {
    if !height.is_finite() || !line_h.is_finite() || line_h <= 0.0 {
        return 1;
    }
    let rows = ((height - 2.0 * CODE_PAD_Y) / line_h).floor();
    if rows >= 1.0 { rows as usize } else { 1 }
}

/// The highest first line that still fills a page of `rows`.
pub(crate) fn last_top(rows: usize, total: usize) -> usize {
    total.saturating_sub(rows)
}

/// The first line after a scroll key, or `None` for a key that does not
/// scroll. A page keeps one line of the last page in view.
pub(crate) fn scroll_key(key: &str, top: usize, rows: usize, total: usize) -> Option<usize> {
    let page = rows.saturating_sub(1).max(1);
    let to = match key {
        "up" => top.saturating_sub(1),
        "down" => top.saturating_add(1),
        "pageup" => top.saturating_sub(page),
        "pagedown" => top.saturating_add(page),
        "home" => 0,
        "end" => usize::MAX,
        _ => return None,
    };
    Some(to.min(last_top(rows, total)))
}

/// The scrollbar's width at the right edge of the code area.
const SCROLLBAR_W: f32 = 10.0;
/// The thumb's width, centred in the scrollbar.
const THUMB_W: f32 = 6.0;
/// The shortest thumb, so a long file still leaves one to grab.
const MIN_THUMB_H: f32 = 24.0;
const SCROLLBAR_GROUP: &str = "file-scrollbar";

/// Where the thumb sits in the scrollbar's track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScrollThumb {
    /// Its top edge, in pixels below the track's top.
    pub(crate) top: f32,
    pub(crate) height: f32,
}

/// The thumb in a track `track` pixels tall for a page of `rows` lines
/// from line `top` of `total`, or `None` when the whole file fits.
pub(crate) fn scroll_thumb(
    track: f32,
    top: usize,
    rows: usize,
    total: usize,
) -> Option<ScrollThumb> {
    let max_top = last_top(rows, total);
    if !track.is_finite() || track <= 0.0 || rows == 0 || max_top == 0 {
        return None;
    }
    let track = f64::from(track);
    let height = (track * rows as f64 / total as f64)
        .max(f64::from(MIN_THUMB_H))
        .min(track);
    let top = (track - height) * top.min(max_top) as f64 / max_top as f64;
    Some(ScrollThumb {
        top: top as f32,
        height: height as f32,
    })
}

/// The first line once a drag has moved the thumb `dy` pixels from where
/// it sat at line `from`.
pub(crate) fn dragged_line(track: f32, from: usize, dy: f32, rows: usize, total: usize) -> usize {
    let max_top = last_top(rows, total);
    let Some(thumb) = scroll_thumb(track, from, rows, total) else {
        return 0;
    };
    let room = f64::from(track) - f64::from(thumb.height);
    if room <= 0.0 || !dy.is_finite() {
        return from.min(max_top);
    }
    let at = ((f64::from(thumb.top) + f64::from(dy)) / room).clamp(0.0, 1.0);
    (at * max_top as f64).round() as usize
}

/// What a press at `y` pixels below the track's top hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackHit {
    PageUp,
    Thumb,
    PageDown,
}

pub(crate) fn track_hit(y: f32, thumb: ScrollThumb) -> TrackHit {
    if y < thumb.top {
        TrackHit::PageUp
    } else if y >= thumb.top + thumb.height {
        TrackHit::PageDown
    } else {
        TrackHit::Thumb
    }
}

/// What a drag of a scrollbar thumb carries: its pane.
struct ThumbDrag(PaneId);

/// A thumb drag draws nothing under the pointer.
struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Where the pointer took the thumb: its window `y` and the first line
/// shown then.
#[derive(Debug, Clone, Copy)]
struct ThumbGrab {
    y: f32,
    from: usize,
}

/// Events a [`FilePaneView`] emits to the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FilePaneEvent {
    /// A right click asks for the pane actions menu at this window position.
    ContextMenu { at: Point<Pixels> },
}

impl EventEmitter<FilePaneEvent> for FilePaneView {}

pub(crate) struct FilePaneView {
    id: PaneId,
    link: Rc<Link>,
    focus: FocusHandle,
    family: SharedString,
    font_size: f32,
    theme: Theme,
    wrap: bool,
    frame: Option<FileFrame>,
    frame_at_ms: Option<u64>,
    test_control: bool,
    /// The lines the code area showed at its last paint.
    rows: Option<usize>,
    /// The rows the core was last told, so a resize is sent once.
    sent_rows: Option<usize>,
    wheel_rest: f32,
    /// The code area's window top and height at its last paint: the
    /// scrollbar's track.
    track: Option<(f32, f32)>,
    /// The thumb the pointer holds, from its press to its release.
    grab: Option<ThumbGrab>,
    /// Where each rendered Markdown row lay at the last paint.
    row_spans: RowSpans,
    /// Where rendered Markdown is scrolled to: `row` counts from the
    /// document's first row. Rows differ in height, so it scrolls by pixels.
    pos: ScrollPos,
    /// Rendered Markdown shows its end, aligned to the bottom, until the
    /// rows there are measured.
    pin_end: bool,
}

impl FilePaneView {
    #[allow(
        clippy::too_many_arguments,
        reason = "one call site; each argument is a separate part of the file pane's setup"
    )]
    pub(crate) fn new(
        id: PaneId,
        link: Rc<Link>,
        family: Option<SharedString>,
        font_size: f32,
        theme: Theme,
        wrap: bool,
        test_control: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            id,
            link,
            focus: cx.focus_handle(),
            family: family.unwrap_or_else(|| SharedString::new_static(FALLBACK_MONO)),
            font_size,
            theme,
            wrap,
            frame: None,
            frame_at_ms: None,
            test_control,
            rows: None,
            sent_rows: None,
            wheel_rest: 0.0,
            track: None,
            grab: None,
            row_spans: RowSpans::default(),
            pos: ScrollPos::default(),
            pin_end: false,
        }
    }

    pub(crate) fn handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub(crate) fn set_frame(&mut self, frame: FileFrame, cx: &mut Context<Self>) {
        self.frame_at_ms = stamp_frame(self.test_control);
        let same_view = self.frame.as_ref().is_some_and(|f| {
            f.path == frame.path && f.rendered.is_some() == frame.rendered.is_some()
        });
        if let Some(rendered) = &frame.rendered {
            let holds_pos =
                (frame.top_line..frame.top_line + rendered.len()).contains(&self.pos.row);
            if !same_view || (!self.pin_end && !holds_pos) {
                self.pos = ScrollPos {
                    row: frame.top_line,
                    offset: 0.0,
                };
                self.pin_end = false;
            }
        } else {
            self.pin_end = false;
        }
        self.frame = Some(frame);
        cx.notify();
    }

    pub(crate) fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        if self.wrap != wrap {
            self.wrap = wrap;
            cx.notify();
        }
    }

    pub(crate) fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub(crate) fn set_font(
        &mut self,
        family: Option<SharedString>,
        size: f32,
        cx: &mut Context<Self>,
    ) {
        self.family = family.unwrap_or_else(|| SharedString::new_static(FALLBACK_MONO));
        self.font_size = size;
        cx.notify();
    }

    /// What `test-control` reads for `state` with `text: true`.
    pub(crate) fn facts(&self, with_text: bool) -> Option<TerminalFacts> {
        let frame = self.frame.as_ref()?;
        let content = content_len(frame);
        let rows = with_text.then(|| {
            if content == 0
                && let Some(notice) = &frame.notice
            {
                vec![notice_label(notice)]
            } else if let Some(rendered) = &frame.rendered {
                rendered.iter().map(MdRow::plain_text).collect()
            } else {
                frame.lines.iter().map(|l| l.text.clone()).collect()
            }
        });
        let (skip, shown) = match (frame.rendered.as_ref(), self.anchor()) {
            (Some(_), Some(k)) => {
                let shown = self.rendered_layout().map_or(content - k, |(h, avail)| {
                    shown_rows(&h, self.local_pos(k), avail)
                });
                (k, shown)
            }
            (Some(_), None) => (0, content),
            (None, _) => (0, self.rows.map_or(content, |r| r.min(content))),
        };
        let rows = rows.map(|mut rows| {
            if content > 0 {
                rows.drain(..skip.min(rows.len()));
                rows.truncate(shown);
            }
            rows
        });
        let height = u16::try_from(self.rows.unwrap_or(shown))
            .unwrap_or(u16::MAX)
            .max(1);
        Some(TerminalFacts {
            size: (80, height),
            frame_at_ms: self.frame_at_ms,
            rows,
            scroll: Some(FileScroll {
                top_line: self.top_and_total().0,
                rows: self.page_rows(),
                total_lines: frame.total_lines,
            }),
        })
    }

    /// The code font size and line height.
    fn code_metrics(&self) -> (f32, f32) {
        let font_size = (self.font_size - 1.5).max(10.0);
        (font_size, (font_size * 1.44).round().max(15.0))
    }

    /// The lines a page holds: the measured ones, or the frame's before the
    /// first paint. Rendered Markdown counts the rows wholly in view.
    fn page_rows(&self) -> usize {
        if let (Some(k), Some((h, avail))) = (self.anchor(), self.rendered_layout()) {
            return whole_rows(&h, self.local_pos(k), avail).max(1);
        }
        self.rows
            .or_else(|| self.frame.as_ref().map(content_len))
            .unwrap_or(1)
            .max(1)
    }

    fn rendered(&self) -> bool {
        self.frame.as_ref().is_some_and(|f| f.rendered.is_some())
    }

    /// The first line, or for rendered Markdown the row at the top edge,
    /// and how many there are.
    fn top_and_total(&self) -> (usize, usize) {
        match &self.frame {
            Some(f) if f.rendered.is_some() => (self.pos.row, f.total_lines),
            Some(f) => (f.top_line, f.total_lines),
            None => (0, 0),
        }
    }

    /// The scroll position's row as an index into the frame's rendered
    /// rows, when the frame holds it.
    fn anchor(&self) -> Option<usize> {
        let frame = self.frame.as_ref()?;
        let len = frame.rendered.as_ref()?.len();
        self.pos
            .row
            .checked_sub(frame.top_line)
            .filter(|k| *k < len)
    }

    fn local_pos(&self, k: usize) -> ScrollPos {
        ScrollPos {
            row: k,
            offset: self.pos.offset,
        }
    }

    /// The rendered body's height for rows, between its paddings.
    fn rendered_avail(&self) -> Option<f32> {
        let (_, height) = self.track?;
        Some((height - BODY_PAD_TOP - BODY_PAD_BOTTOM).max(1.0))
    }

    /// The measured heights of the frame's rendered rows and the height
    /// they are seen through, once all were drawn.
    fn rendered_layout(&self) -> Option<(Vec<f32>, f32)> {
        let len = self.frame.as_ref()?.rendered.as_ref()?.len();
        let h = heights(&self.row_spans.borrow(), len)?;
        Some((h, self.rendered_avail()?))
    }

    /// The rendered rows asked for below the scroll position: enough of the
    /// shortest row, a prose line at the UI size or a code line, to fill
    /// the view.
    fn rendered_rows(&self) -> usize {
        let shortest = prose_line_h(&self.theme).min(self.code_metrics().1);
        self.rendered_avail()
            .map_or(DEFAULT_RENDERED_ROWS, |avail| rows_for(avail, shortest))
    }

    /// Asks for a page of rows above the scroll position and a page below,
    /// so scrolling either way has rows to draw.
    fn request_rendered(&mut self, cx: &mut Context<Self>) {
        let rows = self.rendered_rows();
        let back = rows.min(self.pos.row);
        if self.send_viewport(self.pos.row - back, back + rows, cx) {
            self.sent_rows = Some(rows);
        }
    }

    /// Scrolls rendered Markdown `by` pixels; nothing until its rows were
    /// measured.
    fn scroll_rendered(&mut self, by: f32, cx: &mut Context<Self>) {
        let (Some(k), Some((h, avail))) = (self.anchor(), self.rendered_layout()) else {
            return;
        };
        let top = self.frame.as_ref().map_or(0, |f| f.top_line);
        let Some(next) = scroll_rows(&h, self.local_pos(k), by, avail) else {
            return;
        };
        let next = ScrollPos {
            row: top + next.row,
            offset: next.offset,
        };
        if next == self.pos {
            return;
        }
        let moved = next.row != self.pos.row;
        self.pos = next;
        self.pin_end = false;
        if moved {
            self.request_rendered(cx);
        }
        cx.notify();
    }

    /// Puts rendered row `row` at the top edge.
    fn jump_rendered(&mut self, row: usize, cx: &mut Context<Self>) {
        let (_, total) = self.top_and_total();
        self.pos = ScrollPos {
            row: row.min(total.saturating_sub(1)),
            offset: 0.0,
        };
        self.pin_end = false;
        self.request_rendered(cx);
        cx.notify();
    }

    /// Shows the end of rendered Markdown: at once when the frame holds it,
    /// else bottom-aligned until the rows there are measured.
    fn end_rendered(&mut self, cx: &mut Context<Self>) {
        let Some(frame) = &self.frame else {
            return;
        };
        let len = frame.rendered.as_ref().map_or(0, Vec::len);
        let total = frame.total_lines;
        if frame.top_line + len >= total && self.rendered_layout().is_some() {
            self.scroll_rendered(f32::MAX, cx);
            return;
        }
        let rows = 2 * self.rendered_rows();
        self.pin_end = true;
        self.pos = ScrollPos {
            row: total.saturating_sub(1),
            offset: 0.0,
        };
        self.send_viewport(total.saturating_sub(rows), rows, cx);
        cx.notify();
    }

    /// Scrolls rendered Markdown for a key; `false` for a key that does not
    /// scroll.
    fn rendered_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let line = prose_line_h(&self.theme);
        let page = self.rendered_avail().map_or(line, |a| (a - line).max(line));
        match key {
            "up" => self.scroll_rendered(-line, cx),
            "down" => self.scroll_rendered(line, cx),
            "pageup" => self.scroll_rendered(-page, cx),
            "pagedown" => self.scroll_rendered(page, cx),
            "home" if self.frame.as_ref().is_some_and(|f| f.top_line == 0) => {
                self.scroll_rendered(-f32::MAX, cx);
            }
            "home" => self.jump_rendered(0, cx),
            "end" => self.end_rendered(cx),
            _ => return false,
        }
        true
    }

    /// Once the bottom-aligned end is measured, keeps that place as a
    /// scroll position.
    fn settle_end(&mut self, cx: &mut Context<Self>) {
        if !self.pin_end {
            return;
        }
        let Some((h, avail)) = self.rendered_layout() else {
            return;
        };
        let top = self.frame.as_ref().map_or(0, |f| f.top_line);
        if let Some(end) = scroll_rows(&h, ScrollPos::default(), f32::MAX, avail) {
            self.pos = ScrollPos {
                row: top + end.row,
                offset: end.offset,
            };
            self.pin_end = false;
            self.request_rendered(cx);
            cx.notify();
        }
    }

    /// Takes the code area's bounds at a paint; a new number of rows goes
    /// to the core, which slices its frames to it.
    fn measured(&mut self, top: f32, height: f32, cx: &mut Context<Self>) {
        if self.track.is_none_or(|(_, h)| h != height) {
            cx.notify();
        }
        self.track = Some((top, height));
        let rows = rows_in(height, self.code_metrics().1);
        if self.rows != Some(rows) {
            self.rows = Some(rows);
            cx.notify();
        }
        if self.rendered() {
            self.settle_end(cx);
            if !self.pin_end && self.sent_rows != Some(self.rendered_rows()) {
                self.request_rendered(cx);
            }
            return;
        }
        if self.sent_rows == Some(rows) {
            return;
        }
        let (top, total) = self.top_and_total();
        let top_line = top.min(last_top(rows, total));
        // A refused size is asked for again on the next paint.
        if self.send_viewport(top_line, rows, cx) {
            self.sent_rows = Some(rows);
        }
    }

    fn send_viewport(&self, top_line: usize, rows: usize, cx: &mut Context<Self>) -> bool {
        let msg = ClientMsg::SetFileViewport {
            pane: self.id,
            top_line,
            rows,
        };
        matches!(deliver(&self.link, msg, cx), Sent::Delivered | Sent::Queued)
    }

    fn scroll_to(&mut self, top_line: usize, cx: &mut Context<Self>) {
        let (current, total) = self.top_and_total();
        if total == 0 {
            return;
        }
        let rows = self.page_rows();
        let top_line = top_line.min(last_top(rows, total));
        if top_line != current {
            self.send_viewport(top_line, rows, cx);
        }
    }

    fn scroll_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        let (current, _) = self.top_and_total();
        self.scroll_to(current.saturating_add_signed(delta), cx);
    }

    fn on_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.rendered() {
            let line = prose_line_h(&self.theme);
            let by = match event.delta {
                ScrollDelta::Lines(point) => -point.y * line,
                ScrollDelta::Pixels(point) => -f32::from(point.y),
            };
            self.scroll_rendered(by, cx);
            return;
        }
        let line_h = self.code_metrics().1;
        let delta_y: f32 = match event.delta {
            ScrollDelta::Lines(point) => -point.y * line_h,
            ScrollDelta::Pixels(point) => -f32::from(point.y),
        };
        let total_px = self.wheel_rest + delta_y;
        let steps = (total_px / line_h).trunc() as isize;
        self.wheel_rest = total_px - (steps as f32) * line_h;
        if steps != 0 {
            self.scroll_by(steps, cx);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let m = &event.keystroke.modifiers;
        if m.control || m.alt || m.platform || m.function {
            cx.propagate();
            return;
        }
        if self.rendered() {
            if !self.rendered_key(&event.keystroke.key, cx) {
                cx.propagate();
            }
            return;
        }
        let (top, total) = self.top_and_total();
        match scroll_key(&event.keystroke.key, top, self.page_rows(), total) {
            Some(to) => self.scroll_to(to, cx),
            None => cx.propagate(),
        }
    }

    fn on_track_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.grab = None;
        let Some((track_top, track_h)) = self.track else {
            return;
        };
        let (top, total) = self.top_and_total();
        let rows = self.page_rows();
        let Some(thumb) = scroll_thumb(track_h, top, rows, total) else {
            return;
        };
        let y = event.position.y.as_f32();
        let key = match track_hit(y - track_top, thumb) {
            TrackHit::Thumb => {
                self.grab = Some(ThumbGrab { y, from: top });
                cx.notify();
                return;
            }
            TrackHit::PageUp => "pageup",
            TrackHit::PageDown => "pagedown",
        };
        if self.rendered() {
            self.rendered_key(key, cx);
            return;
        }
        if let Some(to) = scroll_key(key, top, rows, total) {
            self.scroll_to(to, cx);
        }
    }

    fn on_track_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.grab.take().is_some() {
            cx.notify();
        }
    }

    fn on_thumb_drag(
        &mut self,
        event: &DragMoveEvent<ThumbDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.drag(cx).0 != self.id {
            return;
        }
        let Some(grab) = self.grab else {
            return;
        };
        let (_, total) = self.top_and_total();
        let to = dragged_line(
            event.bounds.size.height.as_f32(),
            grab.from,
            event.event.position.y.as_f32() - grab.y,
            self.page_rows(),
            total,
        );
        if self.rendered() {
            if to != self.pos.row {
                self.jump_rendered(to, cx);
            }
            return;
        }
        self.scroll_to(to, cx);
    }

    /// The vertical scrollbar over the code area's right padding, or `None`
    /// when the whole file fits.
    fn render_scrollbar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, track_h) = self.track?;
        let frame = self.frame.as_ref().filter(|f| content_len(f) > 0)?;
        let rows = self.page_rows();
        let (top, _) = self.top_and_total();
        let thumb = scroll_thumb(track_h, top, rows, frame.total_lines)?;
        let t = self.theme;
        let thumb = div()
            .id(("file-scroll-thumb", self.id.get()))
            .absolute()
            .top(px(thumb.top))
            .left(px((SCROLLBAR_W - THUMB_W) / 2.0))
            .w(px(THUMB_W))
            .h(px(thumb.height))
            .rounded_full()
            .bg(t.scroll_thumb(self.grab.is_some()))
            .group_hover(SCROLLBAR_GROUP, move |s| s.bg(t.scroll_thumb(true)))
            .on_drag(ThumbDrag(self.id), |_, _, _, cx| cx.new(|_| NoGhost));
        Some(
            div()
                .id(("file-scrollbar", self.id.get()))
                .group(SCROLLBAR_GROUP)
                .role(Role::ScrollBar)
                .aria_label("Scroll")
                .aria_orientation(accesskit::Orientation::Vertical)
                .aria_min_numeric_value(0.0)
                .aria_max_numeric_value(last_top(rows, frame.total_lines) as f64)
                .aria_numeric_value(top as f64)
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .w(px(SCROLLBAR_W))
                .bg(t.term_bg)
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_track_down))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_track_up))
                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_track_up))
                .on_drag_move(cx.listener(Self::on_thumb_drag))
                .child(thumb)
                .into_any_element(),
        )
    }

    /// The rendered rows: from the scroll position down, drawn from the top
    /// edge less its offset; the rows above it, drawn up from that same
    /// line, so neither waits on a measurement. At the pinned end all rows
    /// sit on the bottom edge.
    fn render_rendered(&self, rendered: &[MdRow], top_line: usize, look: &Look) -> AnyElement {
        let t = &look.theme;
        {
            let mut spans = self.row_spans.borrow_mut();
            spans.clear();
            spans.resize(rendered.len(), (f32::NAN, f32::NAN));
        }
        let mut rows: Vec<AnyElement> = rendered
            .iter()
            .enumerate()
            .map(|(k, row)| {
                file_markdown::render_row(row, k, top_line + k == 0, look, &self.row_spans)
            })
            .collect();
        let column = || {
            v_flex()
                .absolute()
                .left(px(BODY_PAD_X))
                .right(px(BODY_PAD_X.max(SCROLLBAR_W)))
        };
        let body = div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .text_size(t.text(13.0))
            .line_height(relative(1.55))
            .text_color(t.term_text);
        if self.pin_end {
            return body
                .child(column().bottom(px(BODY_PAD_BOTTOM)).children(rows))
                .into_any_element();
        }
        let (k, offset) = self.anchor().map_or((0, 0.0), |k| (k, self.pos.offset));
        let below = rows.split_off(k);
        let edge = BODY_PAD_TOP - offset;
        body.child(column().top(px(edge)).children(below))
            .when_some(self.track.filter(|_| !rows.is_empty()), |d, (_, height)| {
                d.child(column().bottom(px(height - edge)).children(rows))
            })
            .into_any_element()
    }

    fn render_line(&self, line: &FileLine, gutter_w: f32) -> AnyElement {
        let t = self.theme;
        let text = if line.text.is_empty() {
            SharedString::new_static(" ")
        } else {
            SharedString::from(line.text.clone())
        };
        let mut highlights: Vec<(Range<usize>, HighlightStyle)> =
            Vec::with_capacity(line.spans.len());
        for span in &line.spans {
            if span.start < span.end
                && span.end <= line.text.len()
                && line.text.is_char_boundary(span.start)
                && line.text.is_char_boundary(span.end)
            {
                let style = HighlightStyle {
                    color: Some(t.syntax_color(span.kind).into()),
                    ..HighlightStyle::default()
                };
                highlights.push((span.start..span.end, style));
            }
        }
        let styled = if highlights.is_empty() {
            StyledText::new(text).into_any_element()
        } else {
            StyledText::new(text)
                .with_highlights(highlights)
                .into_any_element()
        };
        h_flex()
            .w_full()
            .items_start()
            .child(
                div()
                    .flex_none()
                    .w(px(gutter_w))
                    .pr(px(GUTTER_PAD_RIGHT))
                    .mr(px(10.0))
                    .border_r_1()
                    .border_color(t.term_line)
                    .text_right()
                    .text_color(t.term_faint)
                    .child(line.number.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr(px(SCROLLBAR_W))
                    .when(!self.wrap, |d| d.whitespace_nowrap().overflow_x_hidden())
                    .child(styled),
            )
            .into_any_element()
    }
}

impl Render for FilePaneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let (font_size, line_h) = self.code_metrics();
        let total_lines = self.frame.as_ref().map_or(0, |f| f.total_lines);
        let text = window.text_system();
        let digit_w = text
            .ch_advance(text.resolve_font(&font(self.family.clone())), px(font_size))
            .ok()
            .map(|w| w.as_f32());
        let gutter_w = gutter_width(total_lines, digit_w, font_size);

        let banner = self
            .frame
            .as_ref()
            .and_then(|f| f.notice.as_ref())
            .filter(|_| self.frame.as_ref().is_some_and(|f| content_len(f) > 0))
            .map(|notice| {
                div()
                    .flex_none()
                    .px(px(10.0))
                    .py(px(4.0))
                    .border_b_1()
                    .border_color(t.term_line)
                    .bg(t.term_raised)
                    .text_size(t.text(11.0))
                    .text_color(t.ochre)
                    .child(notice_label(notice))
            });

        let body = match &self.frame {
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(t.text(12.0))
                .text_color(t.term_faint)
                .child("Loading…")
                .into_any_element(),
            Some(frame) if content_len(frame) == 0 => {
                let msg = frame
                    .notice
                    .as_ref()
                    .map_or_else(|| "Empty file.".to_owned(), notice_label);
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(16.0))
                    .text_size(t.text(12.0))
                    .text_color(t.term_muted)
                    .child(msg)
                    .into_any_element()
            }
            Some(FileFrame {
                rendered: Some(rendered),
                top_line,
                ..
            }) => {
                let look = Look {
                    theme: t,
                    mono: self.family.clone(),
                    code_size: font_size,
                    code_line_h: line_h,
                    wrap: self.wrap,
                };
                self.render_rendered(rendered, *top_line, &look)
            }
            Some(frame) => {
                let rows: Vec<AnyElement> = frame
                    .lines
                    .iter()
                    .take(self.rows.unwrap_or(usize::MAX))
                    .map(|line| self.render_line(line, gutter_w))
                    .collect();
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .py(px(CODE_PAD_Y))
                    .overflow_hidden()
                    .text_size(px(font_size))
                    .line_height(px(line_h))
                    .font_family(self.family.clone())
                    .text_color(t.term_text)
                    .children(rows)
                    .into_any_element()
            }
        };

        let view = cx.entity().downgrade();
        let measure = canvas(
            move |bounds, _, cx| {
                let (top, height) = (bounds.origin.y.as_f32(), bounds.size.height.as_f32());
                let _ = view.update(cx, |this, cx| this.measured(top, height, cx));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let scrollbar = self.render_scrollbar(cx);
        let label = self.frame.as_ref().map_or_else(
            || "File preview".to_owned(),
            |f| format!("{}, read-only", f.path),
        );

        v_flex()
            .id(("file-pane-body", self.id.get()))
            .role(Role::Document)
            .aria_label(label)
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(t.term_bg)
            .on_scroll_wheel(cx.listener(Self::on_wheel))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    deliver(
                        &this.link,
                        ClientMsg::Command(Command::FocusPane { pane: this.id }),
                        cx,
                    );
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, event: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    cx.emit(FilePaneEvent::ContextMenu { at: event.position });
                }),
            )
            .children(banner)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(body)
                    .child(measure)
                    .children(scrollbar),
            )
    }
}

/// Rendered rows asked for before the pane's height is known.
const DEFAULT_RENDERED_ROWS: usize = 40;

/// The lines or rendered rows a frame carries.
fn content_len(frame: &FileFrame) -> usize {
    frame.rendered.as_ref().map_or(frame.lines.len(), Vec::len)
}

pub(crate) fn notice_label(notice: &FileNotice) -> String {
    match notice {
        FileNotice::Loading => "Loading…".to_owned(),
        FileNotice::Binary { bytes } => {
            format!("Binary file ({bytes} bytes) — cannot be shown as text.")
        }
        FileNotice::Truncated {
            bytes_shown,
            total_bytes,
            lines_shown,
            capped_by_lines,
        } => {
            if *capped_by_lines {
                format!("Showing first {lines_shown} lines ({bytes_shown} of {total_bytes} bytes).")
            } else {
                format!("Showing first {bytes_shown} of {total_bytes} bytes ({lines_shown} lines).")
            }
        }
        FileNotice::Error(message) => message.clone(),
    }
}

fn stamp_frame(enabled: bool) -> Option<u64> {
    if !enabled {
        return None;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
}

#[cfg(test)]
mod tests {
    use super::{
        MIN_THUMB_H, ScrollThumb, TrackHit, dragged_line, gutter_width, last_top, rows_in,
        scroll_key, scroll_thumb, track_hit,
    };

    #[test]
    fn rf_ctx_02_the_rows_follow_the_code_area_height() {
        // 4 px above and below the lines.
        assert_eq!(rows_in(8.0 + 28.0 * 15.0, 15.0), 28);
        assert_eq!(rows_in(8.0 + 28.0 * 15.0 - 1.0, 15.0), 27);
        assert_eq!(rows_in(8.0 + 90.0 * 18.0, 18.0), 90);
        for (height, line_h) in [(0.0, 15.0), (-5.0, 15.0), (f32::NAN, 15.0), (300.0, 0.0)] {
            assert_eq!(rows_in(height, line_h), 1, "{height} {line_h}");
        }
    }

    #[test]
    fn rf_ctx_02_page_keys_move_by_the_visible_rows() {
        let (rows, total) = (28, 1000);
        assert_eq!(scroll_key("pagedown", 0, rows, total), Some(27));
        assert_eq!(scroll_key("pageup", 27, rows, total), Some(0));
        assert_eq!(scroll_key("pagedown", 0, 90, total), Some(89));
        assert_eq!(scroll_key("down", 5, rows, total), Some(6));
        assert_eq!(scroll_key("up", 0, rows, total), Some(0));
        assert_eq!(scroll_key("x", 5, rows, total), None);
    }

    #[test]
    fn rf_ctx_02_end_shows_a_full_last_page_and_home_the_top() {
        assert_eq!(scroll_key("end", 0, 28, 1000), Some(972));
        assert_eq!(scroll_key("pagedown", 990, 28, 1000), Some(972));
        assert_eq!(scroll_key("home", 500, 28, 1000), Some(0));
        // A file shorter than the pane never scrolls.
        assert_eq!(scroll_key("end", 0, 90, 40), Some(0));
        assert_eq!(last_top(90, 40), 0);
    }

    #[test]
    fn rf_ctx_02_the_gutter_fits_the_widest_line_number() {
        // 9 px of padding and a 1 px border right of the numbers.
        let w = |total, digit| gutter_width(total, Some(digit), 13.0);
        assert_eq!(w(0, 7.0), 31.0);
        assert_eq!(w(1, 7.0), 31.0);
        assert_eq!(w(999, 7.0), 31.0);
        assert_eq!(w(1_000, 7.0), 38.0);
        assert_eq!(w(9_999, 7.0), 38.0);
        assert_eq!(w(10_000, 7.0), 45.0);
        assert_eq!(w(99_999, 7.0), 45.0);
        assert_eq!(w(123_456, 7.0), 52.0);
        // A fractional advance rounds up so the last digit is never clipped.
        assert_eq!(w(123_456, 7.2), 54.0);
        // Zoom: the width follows the measured advance, digit for digit.
        assert_eq!(w(123_456, 12.0), 82.0);
        assert!(w(123_456, 12.0) > w(123_456, 7.2));
        // No metrics: 0.6 of the font size per digit.
        for bad in [None, Some(0.0), Some(-1.0), Some(f32::NAN)] {
            assert_eq!(gutter_width(10_000, bad, 20.0), 70.0, "{bad:?}");
        }
    }

    #[test]
    fn rf_ctx_02_the_thumb_is_the_visible_share_of_the_track() {
        // 30 of 300 lines in a 600 px track: a tenth of it.
        let at = |top| scroll_thumb(600.0, top, 30, 300).unwrap();
        assert_eq!(
            at(0),
            ScrollThumb {
                top: 0.0,
                height: 60.0
            }
        );
        // Halfway down the scrollable range, halfway down the free track.
        assert_eq!(at(135).top, 270.0);
        // The last page puts the thumb against the bottom.
        assert_eq!(
            at(270),
            ScrollThumb {
                top: 540.0,
                height: 60.0
            }
        );
        // A top past the last page (the core clamps it) still stays inside.
        assert_eq!(at(10_000).top, 540.0);
    }

    #[test]
    fn rf_ctx_02_the_thumb_never_shrinks_below_its_minimum() {
        let thumb = scroll_thumb(600.0, 0, 30, 1_000_000).unwrap();
        assert_eq!(thumb.height, MIN_THUMB_H);
        let last = scroll_thumb(600.0, 999_970, 30, 1_000_000).unwrap();
        assert_eq!(last.top + last.height, 600.0);
        // A track shorter than the minimum is filled, never overflowed.
        let tiny = scroll_thumb(10.0, 5, 2, 100).unwrap();
        assert_eq!(
            tiny,
            ScrollThumb {
                top: 0.0,
                height: 10.0
            }
        );
    }

    #[test]
    fn rf_ctx_02_a_file_that_fits_the_pane_has_no_thumb() {
        assert_eq!(scroll_thumb(600.0, 0, 30, 30), None);
        assert_eq!(scroll_thumb(600.0, 0, 30, 12), None);
        assert_eq!(scroll_thumb(600.0, 0, 30, 0), None);
        for track in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(scroll_thumb(track, 0, 30, 300), None, "{track}");
        }
    }

    #[test]
    fn rf_ctx_02_dragging_the_thumb_scrolls_in_step_with_it() {
        let (track, rows, total) = (600.0, 30, 300);
        // Not moved: the same line, for every line.
        for from in 0..=270 {
            assert_eq!(dragged_line(track, from, 0.0, rows, total), from);
        }
        // 540 px of free track for 270 lines: 2 px a line.
        assert_eq!(dragged_line(track, 0, 20.0, rows, total), 10);
        assert_eq!(dragged_line(track, 100, -40.0, rows, total), 80);
        // Past either end of the track it stops at the first or last page.
        assert_eq!(dragged_line(track, 10, -5_000.0, rows, total), 0);
        assert_eq!(dragged_line(track, 10, 5_000.0, rows, total), 270);
        // The minimum thumb on a huge file still reaches both ends.
        let total = 1_000_000;
        assert_eq!(dragged_line(track, 0, 576.0, rows, total), 999_970);
        assert_eq!(dragged_line(track, 999_970, -576.0, rows, total), 0);
        // A thumb that fills its track cannot move the page.
        assert_eq!(dragged_line(10.0, 5, 3.0, 2, 100), 5);
        assert_eq!(dragged_line(track, 0, 50.0, 30, 30), 0);
    }

    #[test]
    fn rf_ctx_02_a_press_on_the_track_pages_toward_it() {
        let thumb = ScrollThumb {
            top: 100.0,
            height: 60.0,
        };
        assert_eq!(track_hit(0.0, thumb), TrackHit::PageUp);
        assert_eq!(track_hit(99.5, thumb), TrackHit::PageUp);
        assert_eq!(track_hit(100.0, thumb), TrackHit::Thumb);
        assert_eq!(track_hit(159.5, thumb), TrackHit::Thumb);
        assert_eq!(track_hit(160.0, thumb), TrackHit::PageDown);
        assert_eq!(track_hit(599.0, thumb), TrackHit::PageDown);
    }
}
