//! One terminal pane: draws the core's frame and turns keys, IME text, the
//! mouse and pastes into messages for the core. The UI keeps the visible
//! rows only (ADR-06).

use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::h_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_input::Mods;
use yien_input::mouse::{self, Action, Button};
use yien_proto::frame::{Frame, Modes, Rgb, SelectionKind, ViewportPoint};
use yien_proto::{ClientMsg, Command, InputBytes, PaneId};

use crate::actions::{CopySelection, PasteClipboard, TERMINAL_CONTEXT};
use crate::bridge::{Backlog, Link, deliver, pane_busy};
use crate::grid::{self, CursorPaint, PastePlan, PromptKey, Underline, WheelAction};
use crate::theme::Theme;

/// The terminal font size, before display scaling (RNF-19).
const FONT_SIZE: f32 = 13.0;
/// Used when none of the preferred mono families is installed: the UI
/// default is proportional, and a grid needs a fixed advance.
const FALLBACK_MONO: &str = "Consolas";
/// How long a size must hold before the pane is resized again.
const RESIZE_SETTLE: Duration = Duration::from_millis(60);

/// The cell size in logical pixels, snapped to device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Metrics {
    cell_w: f32,
    cell_h: f32,
}

pub(crate) struct TerminalPane {
    id: PaneId,
    link: Rc<Link>,
    focus: FocusHandle,
    family: SharedString,
    theme: Theme,
    frame: Option<Frame>,
    /// IME composition not yet committed (RF-PAN-15).
    preedit: Option<String>,
    /// The grid's bounds and cell size at the last paint.
    grid: Option<(Bounds<Pixels>, Metrics)>,
    sent_size: Option<(u16, u16)>,
    pending_size: Option<(u16, u16)>,
    resize_task: Option<Task<()>>,
    wheel_rest: f32,
    selecting: bool,
    /// A press reported to a program that asked for the mouse.
    reported: Option<Button>,
    last_motion: Option<ViewportPoint>,
    /// A risky paste waiting for a yes (RF-PAN-09).
    pending_paste: Option<(Vec<u8>, String)>,
    _subscriptions: Vec<Subscription>,
}

impl TerminalPane {
    pub(crate) fn new(
        id: PaneId,
        link: Rc<Link>,
        family: Option<SharedString>,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus, window, |this, _, cx| this.focus_changed(true, cx)),
            cx.on_blur(&focus, window, |this, _, cx| this.focus_changed(false, cx)),
        ];
        Self {
            id,
            link,
            focus,
            family: family.unwrap_or_else(|| SharedString::new_static(FALLBACK_MONO)),
            theme,
            frame: None,
            preedit: None,
            grid: None,
            sent_size: None,
            pending_size: None,
            resize_task: None,
            wheel_rest: 0.0,
            selecting: false,
            reported: None,
            last_motion: None,
            pending_paste: None,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The single place frames enter the view, so a change in how the core
    /// delivers them touches only this.
    pub(crate) fn set_frame(&mut self, frame: Frame, cx: &mut Context<Self>) {
        self.frame = Some(frame);
        cx.notify();
    }

    pub(crate) fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    fn modes(&self) -> Modes {
        self.frame.as_ref().map(|f| f.modes).unwrap_or_default()
    }

    fn send(&self, msg: ClientMsg, cx: &mut Context<Self>) {
        let before = self.link.revision();
        deliver(&self.link, msg, cx);
        if self.link.revision() != before {
            cx.notify();
        }
    }

    fn backlog(&self) -> Backlog {
        self.link.pane_backlog(self.id)
    }

    /// The user's own bytes. A pane scrolled back returns to the live screen
    /// first, so what is typed is seen.
    fn write_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        if self.frame.as_ref().is_some_and(|f| f.display_offset > 0) {
            self.send(ClientMsg::ScrollToBottom { pane: self.id }, cx);
        }
        self.send(
            ClientMsg::Input {
                pane: self.id,
                bytes: InputBytes::new(bytes),
            },
            cx,
        );
    }

    fn write_report(&self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if !bytes.is_empty() {
            self.send(
                ClientMsg::Report {
                    pane: self.id,
                    bytes: InputBytes::new(bytes),
                },
                cx,
            );
        }
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.modes().focus_reporting {
            self.write_report(yien_input::focus_report(focused).to_vec(), cx);
        }
        cx.notify();
    }

    /// Called from paint with the grid's size in cells. The first size goes
    /// out at once so the shell starts at the right width; later ones wait
    /// for the size to settle.
    fn request_size(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let size = (cols, rows);
        if self.pending_size == Some(size)
            || (self.pending_size.is_none() && self.sent_size == Some(size))
        {
            return;
        }
        if self.sent_size.is_none() {
            self.send_size(size, cx);
            return;
        }
        self.pending_size = Some(size);
        self.resize_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_SETTLE).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(size) = this.pending_size.take() {
                    this.send_size(size, cx);
                }
            });
        }));
    }

    fn send_size(&mut self, (cols, rows): (u16, u16), cx: &mut Context<Self>) {
        self.pending_size = None;
        if self.sent_size == Some((cols, rows)) {
            return;
        }
        self.sent_size = Some((cols, rows));
        self.send(
            ClientMsg::ResizePane {
                pane: self.id,
                cols,
                rows,
            },
            cx,
        );
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if self.pending_paste.is_some() {
            match grid::paste_prompt_key(&keystroke.key) {
                PromptKey::Confirm => self.confirm_paste(cx),
                PromptKey::Cancel => self.cancel_paste(cx),
                PromptKey::Swallow => {}
            }
            cx.stop_propagation();
            return;
        }
        let press = grid::KeyPress {
            key: &keystroke.key,
            key_char: keystroke.key_char.as_deref(),
            mods: mods_of(&keystroke.modifiers),
            platform: keystroke.modifiers.platform,
            prefer_text: event.prefer_character_input,
        };
        if let Some(bytes) = grid::key_bytes(&press, self.modes().app_cursor) {
            self.write_input(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn copy(&mut self, _: &CopySelection, _: &mut Window, cx: &mut Context<Self>) {
        self.send(ClientMsg::CopySelection { pane: self.id }, cx);
    }

    fn paste(&mut self, _: &PasteClipboard, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        match grid::paste_plan(&text, self.modes().bracketed_paste) {
            PastePlan::Nothing => {}
            PastePlan::Send(bytes) => self.write_input(bytes, cx),
            PastePlan::Confirm { bytes, reason } => {
                self.pending_paste = Some((bytes, reason));
                cx.notify();
            }
        }
    }

    fn confirm_paste(&mut self, cx: &mut Context<Self>) {
        if let Some((bytes, _)) = self.pending_paste.take() {
            self.write_input(bytes, cx);
        }
        cx.notify();
    }

    fn cancel_paste(&mut self, cx: &mut Context<Self>) {
        self.pending_paste = None;
        cx.notify();
    }

    /// The cell under a window position, with the half it falls on.
    fn cell_at(&self, position: Point<Pixels>) -> Option<(ViewportPoint, yien_proto::frame::Side)> {
        let (bounds, m) = self.grid?;
        let frame = self.frame.as_ref()?;
        let local = position - bounds.origin;
        Some(grid::cell_at(
            local.x.as_f32(),
            local.y.as_f32(),
            m.cell_w,
            m.cell_h,
            frame.size.cols,
            frame.size.rows,
        ))
    }

    fn mouse_report(
        &self,
        button: Button,
        action: Action,
        at: ViewportPoint,
        mods: Mods,
    ) -> Option<Vec<u8>> {
        let modes = self.modes();
        let event = mouse::MouseEvent {
            button,
            action,
            col: at.col,
            row: at.row,
            mods,
        };
        mouse::report(&event, grid::tracking(&modes), grid::encoding(&modes))
    }

    /// Whether the program owns the mouse; Shift hands it back to the terminal.
    fn program_owns_mouse(&self, modifiers: &Modifiers) -> bool {
        self.modes().mouse != yien_proto::frame::MouseTracking::Off && !modifiers.shift
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.send(ClientMsg::Command(Command::FocusPane { pane: self.id }), cx);
        let Some((at, side)) = self.cell_at(event.position) else {
            return;
        };
        let mods = mods_of(&event.modifiers);
        let button = match event.button {
            MouseButton::Left => Button::Left,
            MouseButton::Middle => Button::Middle,
            MouseButton::Right => Button::Right,
            _ => return,
        };
        if self.program_owns_mouse(&event.modifiers) {
            if let Some(bytes) = self.mouse_report(button, Action::Press, at, mods) {
                self.reported = Some(button);
                self.last_motion = Some(at);
                self.write_report(bytes, cx);
            }
            return;
        }
        if button != Button::Left {
            return;
        }
        let kind = if event.modifiers.alt {
            SelectionKind::Block
        } else {
            match event.click_count {
                2 => SelectionKind::Semantic,
                n if n >= 3 => SelectionKind::Lines,
                _ => SelectionKind::Simple,
            }
        };
        self.selecting = true;
        self.send(
            ClientMsg::StartSelection {
                pane: self.id,
                at,
                side,
                kind,
            },
            cx,
        );
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((at, side)) = self.cell_at(event.position) else {
            return;
        };
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            self.send(
                ClientMsg::UpdateSelection {
                    pane: self.id,
                    at,
                    side,
                },
                cx,
            );
            return;
        }
        // Motion to a pane that is not reading is dropped rather than queued:
        // only the latest position matters, and it would crowd out keys.
        if !self.program_owns_mouse(&event.modifiers)
            || self.last_motion == Some(at)
            || self.backlog() != Backlog::Clear
        {
            return;
        }
        self.last_motion = Some(at);
        let button = match event.pressed_button {
            Some(MouseButton::Left) => Button::Left,
            Some(MouseButton::Middle) => Button::Middle,
            Some(MouseButton::Right) => Button::Right,
            _ => Button::None,
        };
        if let Some(bytes) =
            self.mouse_report(button, Action::Motion, at, mods_of(&event.modifiers))
        {
            self.write_report(bytes, cx);
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        let Some(button) = self.reported.take() else {
            return;
        };
        let at = self
            .cell_at(event.position)
            .map(|(at, _)| at)
            .or(self.last_motion)
            .unwrap_or(ViewportPoint::new(0, 0));
        if let Some(bytes) =
            self.mouse_report(button, Action::Release, at, mods_of(&event.modifiers))
        {
            self.write_report(bytes, cx);
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((_, m)) = self.grid else {
            return;
        };
        let Some((at, _)) = self.cell_at(event.position) else {
            return;
        };
        let lines = event.delta.pixel_delta(px(m.cell_h)).y.as_f32() / m.cell_h + self.wheel_rest;
        if !lines.is_finite() {
            self.wheel_rest = 0.0;
            return;
        }
        let whole = lines.trunc();
        self.wheel_rest = lines - whole;
        let whole = whole.clamp(-1000.0, 1000.0) as i32;
        match grid::wheel_action(whole, &self.modes(), at, mods_of(&event.modifiers)) {
            Some(WheelAction::Report(bytes)) => self.write_report(bytes, cx),
            Some(WheelAction::Scroll(delta)) => self.send(
                ClientMsg::Scroll {
                    pane: self.id,
                    delta,
                },
                cx,
            ),
            None => {}
        }
    }

    fn render_paste_strip(&self, reason: String, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .role(Role::Button)
                .flex_none()
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(t.term_strong)
                .text_color(t.term_text)
                .cursor_pointer()
                .child(label)
        };
        h_flex()
            .id("paste-confirm")
            .role(Role::Alert)
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(4.0))
            .border_b_1()
            .border_color(t.term_line)
            .bg(t.term_raised)
            .text_size(px(12.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(t.ochre)
                    .child(format!("{reason} Enter pastes, Esc cancels.")),
            )
            .child(
                button("paste-yes", "Paste")
                    .aria_label("Paste anyway")
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_paste(cx))),
            )
            .child(
                button("paste-no", "Cancel")
                    .aria_label("Cancel the paste")
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_paste(cx))),
            )
            .into_any_element()
    }
}

fn mods_of(m: &Modifiers) -> Mods {
    Mods {
        ctrl: m.control,
        alt: m.alt,
        shift: m.shift,
    }
}

fn hsla(c: Rgb) -> Hsla {
    Rgba {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: 1.0,
    }
    .into()
}

fn to_rgb(c: Rgba) -> Rgb {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb::new(byte(c.r), byte(c.g), byte(c.b))
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let background = self
            .frame
            .as_ref()
            .map(|f| hsla(f.background))
            .unwrap_or_else(|| self.theme.term_bg.into());
        let strip = self
            .pending_paste
            .as_ref()
            .map(|(_, reason)| reason.clone())
            .map(|reason| self.render_paste_strip(reason, cx));
        let busy = pane_busy(self.backlog()).map(|text| {
            let t = self.theme;
            div()
                .id("pane-busy")
                .role(Role::Status)
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .px(px(8.0))
                .py(px(4.0))
                .border_t_1()
                .border_color(t.term_line)
                .bg(t.term_raised)
                .text_size(px(12.0))
                .text_color(t.ochre)
                .occlude()
                .child(text)
        });
        div()
            .relative()
            .id(("terminal", self.id.get()))
            .key_context(TERMINAL_CONTEXT)
            .track_focus(&self.focus)
            .role(Role::Terminal)
            .aria_label("Terminal")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .size_full()
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .cursor(CursorStyle::IBeam)
            .children(strip)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(10.0))
                    .py(px(8.0))
                    .bg(background)
                    .when(!focused, |d| d.opacity(0.82))
                    .child(TerminalElement {
                        view: cx.entity(),
                        focused,
                    }),
            )
            .children(busy)
    }
}

struct TerminalElement {
    view: Entity<TerminalPane>,
    focused: bool,
}

struct Prepared {
    quads: Vec<PaintQuad>,
    text: Vec<(Point<Pixels>, ShapedLine)>,
    /// A block cursor and the IME background go under the text, outlines over it.
    under_text: Vec<PaintQuad>,
    over_text: Vec<PaintQuad>,
    line_height: Pixels,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn metrics(window: &Window, family: &SharedString) -> Metrics {
    let text = window.text_system();
    let font_id = text.resolve_font(&font(family.clone()));
    let size = px(FONT_SIZE);
    let advance = text
        .advance(font_id, size, 'm')
        .map(|s| s.width.as_f32())
        .unwrap_or(FONT_SIZE * 0.6);
    let height = text.ascent(font_id, size).as_f32() + text.descent(font_id, size).as_f32().abs();
    let height = if height.is_finite() && height > 0.0 {
        height
    } else {
        FONT_SIZE * 1.2
    };
    let scale = window.scale_factor();
    Metrics {
        cell_w: grid::snap(advance, scale),
        cell_h: grid::snap(height, scale),
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Prepared>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let family = self.view.read(cx).family.clone();
        let m = metrics(window, &family);
        let (cols, rows) = grid::grid_size(
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
            m.cell_w,
            m.cell_h,
        );
        self.view.update(cx, |view, cx| {
            view.grid = Some((bounds, m));
            view.request_size(cols, rows, cx);
        });
        let view = self.view.read(cx);
        let theme = view.theme;
        let frame = view.frame.as_ref()?;
        let preedit = view.preedit.clone();
        let base = font(family);
        let cell = |col: u16, row: usize| {
            point(
                bounds.origin.x + px(f32::from(col) * m.cell_w),
                bounds.origin.y + px(row as f32 * m.cell_h),
            )
        };
        let selection = to_rgb(theme.term_strong);
        let mut quads = Vec::new();
        let mut text = Vec::new();
        let cursor = frame.cursor.and_then(|c| {
            let paint = grid::cursor_paint(&c, self.focused)?;
            Some((c, paint))
        });
        let block_at = cursor.and_then(|(c, paint)| {
            (paint == CursorPaint::Block && preedit.is_none()).then_some(c.point)
        });
        for (row, line) in frame.lines.iter().enumerate() {
            for span in grid::background_spans(line, frame.background, selection) {
                quads.push(fill(
                    Bounds::new(
                        cell(span.col, row),
                        size(px(f32::from(span.cells) * m.cell_w), px(m.cell_h)),
                    ),
                    hsla(span.color),
                ));
            }
            let ink = block_at
                .filter(|p| usize::from(p.row) == row)
                .map(|p| (p.col, frame.background));
            for span in grid::text_spans(line, ink) {
                let color = hsla(span.style.fg);
                let run = TextRun {
                    len: span.text.len(),
                    font: Font {
                        weight: if span.style.bold {
                            FontWeight::BOLD
                        } else {
                            FontWeight::NORMAL
                        },
                        style: if span.style.italic {
                            FontStyle::Italic
                        } else {
                            FontStyle::Normal
                        },
                        ..base.clone()
                    },
                    color,
                    background_color: None,
                    underline: match span.style.underline {
                        Underline::None => None,
                        Underline::Straight | Underline::Curly => Some(UnderlineStyle {
                            thickness: px(1.0),
                            color: Some(color),
                            wavy: span.style.underline == Underline::Curly,
                        }),
                    },
                    strikethrough: span.style.strike.then_some(StrikethroughStyle {
                        thickness: px(1.0),
                        color: Some(color),
                    }),
                };
                let force = (!span.wide).then_some(px(m.cell_w));
                let shaped =
                    window
                        .text_system()
                        .shape_line(span.text.into(), px(FONT_SIZE), &[run], force);
                text.push((cell(span.col, row), shaped));
            }
        }
        let mut under_text = Vec::new();
        let mut over_text = Vec::new();
        if let Some((c, paint)) = cursor {
            let origin = cell(c.point.col, usize::from(c.point.row));
            let cells = grid::cursor_cells(frame, c.point);
            let width = px(f32::from(cells) * m.cell_w);
            let color = hsla(frame.cursor_color);
            let thin = px(grid::snap(2.0, window.scale_factor()));
            let quad = match paint {
                CursorPaint::Block => fill(Bounds::new(origin, size(width, px(m.cell_h))), color),
                CursorPaint::Hollow => outline(
                    Bounds::new(origin, size(width, px(m.cell_h))),
                    color,
                    BorderStyle::Solid,
                ),
                CursorPaint::Beam => fill(Bounds::new(origin, size(thin, px(m.cell_h))), color),
                CursorPaint::Underline => fill(
                    Bounds::new(
                        point(origin.x, origin.y + px(m.cell_h) - thin),
                        size(width, thin),
                    ),
                    color,
                ),
            };
            if preedit.is_none() {
                if paint == CursorPaint::Block {
                    under_text.push(quad);
                } else {
                    over_text.push(quad);
                }
            }
        }
        if let Some(preedit) = preedit.filter(|p| !p.is_empty()) {
            let at = frame
                .cursor
                .map(|c| c.point)
                .unwrap_or(ViewportPoint::new(frame.size.rows.saturating_sub(1), 0));
            let origin = cell(at.col, usize::from(at.row));
            let color: Hsla = theme.term_text.into();
            let run = TextRun {
                len: preedit.len(),
                font: base,
                color,
                background_color: None,
                underline: Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(color),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let shaped =
                window
                    .text_system()
                    .shape_line(preedit.into(), px(FONT_SIZE), &[run], None);
            under_text.push(fill(
                Bounds::new(origin, size(shaped.width, px(m.cell_h))),
                hsla(frame.background),
            ));
            text.push((origin, shaped));
        }
        Some(Prepared {
            quads,
            text,
            under_text,
            over_text,
            line_height: px(m.cell_h),
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepared: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.view.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        let Some(prepared) = prepared.take() else {
            return;
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in prepared.quads {
                window.paint_quad(quad);
            }
            for quad in prepared.under_text {
                window.paint_quad(quad);
            }
            for (origin, line) in prepared.text {
                let _ = line.paint(
                    origin,
                    prepared.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            for quad in prepared.over_text {
                window.paint_quad(quad);
            }
        });
    }
}

impl EntityInputHandler for TerminalPane {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let preedit = self.preedit.as_deref().unwrap_or("");
        let units: Vec<u16> = preedit.encode_utf16().collect();
        let end = range.end.min(units.len());
        let start = range.start.min(end);
        *adjusted_range = Some(start..end);
        Some(String::from_utf16_lossy(&units[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .preedit
            .as_deref()
            .map_or(0, |p| p.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.preedit.as_deref().map(|p| 0..p.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.preedit = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit = None;
        // IME text skips the key handler, so the paste question is held here too.
        if self.pending_paste.is_none() {
            self.write_input(text.as_bytes().to_vec(), cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit =
            (!new_text.is_empty() && self.pending_paste.is_none()).then(|| new_text.to_owned());
        cx.notify();
    }

    /// The IME window opens at the cursor cell, visible or not (RF-PAN-15).
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (_, m) = self.grid?;
        let frame = self.frame.as_ref()?;
        let at = frame
            .cursor
            .map(|c| c.point)
            .unwrap_or(ViewportPoint::new(frame.size.rows.saturating_sub(1), 0));
        Some(Bounds::new(
            point(
                element_bounds.origin.x + px(f32::from(at.col) * m.cell_w),
                element_bounds.origin.y + px(f32::from(at.row) * m.cell_h),
            ),
            size(px(m.cell_w), px(m.cell_h)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
