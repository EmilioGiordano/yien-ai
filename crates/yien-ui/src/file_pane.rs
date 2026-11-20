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

use crate::bridge::{Link, deliver};
use crate::test_control::TerminalFacts;
use crate::theme::Theme;

const FALLBACK_MONO: &str = "Consolas";
const DEFAULT_VISIBLE_ROWS: usize = 60;
const PAGE_SCROLL_MIN: usize = 10;

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
    viewport_rows: usize,
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
            viewport_rows: DEFAULT_VISIBLE_ROWS,
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
        let height = u16::try_from(frame.lines.len()).unwrap_or(u16::MAX).max(1);
        Some(TerminalFacts {
            size: (80, height),
            frame_at_ms: self.frame_at_ms,
            rows,
        })
    }

    fn scroll_to(&mut self, top_line: usize, cx: &mut Context<Self>) {
        let total = self.frame.as_ref().map_or(0, |f| f.total_lines);
        if total == 0 {
            return;
        }
        let max_top = total.saturating_sub(1);
        let top_line = top_line.min(max_top);
        let current = self.frame.as_ref().map_or(0, |f| f.top_line);
        if top_line == current {
            return;
        }
        deliver(
            &self.link,
            ClientMsg::SetFileViewport {
                pane: self.id,
                top_line,
                rows: self.viewport_rows,
            },
            cx,
        );
    }

    fn scroll_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        let current = self.frame.as_ref().map_or(0, |f| f.top_line);
        let next = current.saturating_add_signed(delta);
        self.scroll_to(next, cx);
    }

    fn on_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_h = (self.font_size * 1.44).max(12.0);
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
        let page = self.viewport_rows.saturating_sub(4).max(PAGE_SCROLL_MIN) as isize;
        match event.keystroke.key.as_str() {
            "up" => self.scroll_by(-1, cx),
            "down" => self.scroll_by(1, cx),
            "pageup" => self.scroll_by(-page, cx),
            "pagedown" => self.scroll_by(page, cx),
            "home" => self.scroll_to(0, cx),
            "end" => {
                let total = self.frame.as_ref().map_or(0, |f| f.total_lines);
                self.scroll_to(total.saturating_sub(1), cx);
            }
            _ => cx.propagate(),
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
                    .pr(px(9.0))
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let font_size = (self.font_size - 1.5).max(10.0);
        let line_h = (font_size * 1.44).round().max(15.0);
        let total_lines = self.frame.as_ref().map_or(0, |f| f.total_lines);
        let gutter_w = if total_lines >= 10_000 {
            48.0
        } else if total_lines >= 1_000 {
            42.0
        } else {
            36.0
        };

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
                    .map(|line| self.render_line(line, gutter_w))
                    .collect();
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .py(px(4.0))
                    .overflow_hidden()
                    .text_size(px(font_size))
                    .line_height(px(line_h))
                    .font_family(self.family.clone())
                    .text_color(t.term_text)
                    .children(rows)
                    .into_any_element()
            }
        };

        v_flex()
            .id(("file-pane-body", self.id.get()))
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
            .child(body)
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
