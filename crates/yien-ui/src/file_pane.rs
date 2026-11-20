//! The read-only file preview pane body (RF-CTX-02, ADR-06): draws only the
//! visible line slice from [`FileFrame`], with line numbers, syntax highlights,
//! soft wrap toggle, and wheel/keyboard scrolling.

use std::ops::Range;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::file_preview::{FileFrame, FileLine, FileNotice};
use yien_proto::{ClientMsg, Command, PaneId};

use crate::bridge::{Link, Sent, deliver};
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
        }
    }

    pub(crate) fn handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub(crate) fn set_frame(&mut self, frame: FileFrame, cx: &mut Context<Self>) {
        self.frame_at_ms = stamp_frame(self.test_control);
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
        let rows = with_text.then(|| {
            if frame.lines.is_empty()
                && let Some(notice) = &frame.notice
            {
                vec![notice_label(notice)]
            } else {
                frame.lines.iter().map(|l| l.text.clone()).collect()
            }
        });
        let shown = self
            .rows
            .map_or(frame.lines.len(), |r| r.min(frame.lines.len()));
        let rows = rows.map(|mut rows| {
            if !frame.lines.is_empty() {
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
        })
    }

    /// The code font size and line height.
    fn code_metrics(&self) -> (f32, f32) {
        let font_size = (self.font_size - 1.5).max(10.0);
        (font_size, (font_size * 1.44).round().max(15.0))
    }

    /// The lines a page holds: the measured ones, or the frame's before the
    /// first paint.
    fn page_rows(&self) -> usize {
        self.rows
            .or_else(|| self.frame.as_ref().map(|f| f.lines.len()))
            .unwrap_or(1)
            .max(1)
    }

    fn top_and_total(&self) -> (usize, usize) {
        self.frame
            .as_ref()
            .map_or((0, 0), |f| (f.top_line, f.total_lines))
    }

    /// Takes the code area's height at a paint; a new number of rows goes
    /// to the core, which slices its frames to it.
    fn measured(&mut self, height: f32, cx: &mut Context<Self>) {
        let rows = rows_in(height, self.code_metrics().1);
        if self.rows != Some(rows) {
            self.rows = Some(rows);
            cx.notify();
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
        let (top, total) = self.top_and_total();
        match scroll_key(&event.keystroke.key, top, self.page_rows(), total) {
            Some(to) => self.scroll_to(to, cx),
            None => cx.propagate(),
        }
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
                    .pr(px(10.0))
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
            .filter(|_| !self.frame.as_ref().is_some_and(|f| f.lines.is_empty()))
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
            Some(frame) if frame.lines.is_empty() => {
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
                let height = bounds.size.height.as_f32();
                let _ = view.update(cx, |this, cx| this.measured(height, cx));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
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
                    .child(measure),
            )
    }
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
    use super::{gutter_width, last_top, rows_in, scroll_key};

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
}
