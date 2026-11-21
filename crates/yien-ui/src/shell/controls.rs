//! The settings screen's controls, drawn as the canvas draws them
//! (`design/settings-screen.html`, option A): segmented choices, a switch, a
//! stepper, a select and a text field's frame. Every size is the canvas's at
//! 13 px, scaled with `ui.font.size`. They only draw and report clicks; what
//! a click means is the caller's.

use std::rc::Rc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::toggled;
use crate::assets;
use crate::theme::Theme;

/// What a control does when clicked: called with the part clicked (a
/// choice's index, or 0 and 1 for a stepper's minus and plus).
pub(super) type OnPick = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// The height every control shares.
const HEIGHT: f32 = 24.0;
/// The width of a select and a text field.
pub(super) const FIELD_WIDTH: f32 = 168.0;

fn frame(t: &Theme) -> Div {
    div()
        .h(t.size(HEIGHT))
        .flex_none()
        .border_1()
        .border_color(t.line)
        .rounded(t.size(5.0))
        .bg(t.bg)
}

/// Choices side by side, the chosen one raised: a radio group.
pub(super) fn segmented(
    id: &'static str,
    label: &'static str,
    choices: &[&'static str],
    chosen: usize,
    t: &Theme,
    on_pick: OnPick,
) -> Stateful<Div> {
    let hover = t.hover;
    let items = choices.iter().enumerate().map(|(at, name)| {
        let on = at == chosen;
        let pick = Rc::clone(&on_pick);
        div()
            .id((id, at))
            .role(Role::RadioButton)
            .aria_label(*name)
            .aria_toggled(toggled(on))
            .aria_position_in_set(at + 1)
            .aria_size_of_set(choices.len())
            .flex()
            .items_center()
            .px(t.size(10.0))
            .text_size(t.text(12.0))
            .cursor_pointer()
            .when(at > 0, |d| d.border_l_1().border_color(t.line))
            .map(|d| {
                if on {
                    d.bg(t.hover)
                        .text_color(t.text)
                        .font_weight(FontWeight::MEDIUM)
                } else {
                    d.text_color(t.muted).hover(move |s| s.bg(hover))
                }
            })
            .on_click(move |_, window, cx| pick(at, window, cx))
            .child(*name)
    });
    frame(t)
        .id(id)
        .role(Role::RadioGroup)
        .aria_label(label)
        .flex()
        .items_stretch()
        .overflow_hidden()
        .children(items)
}

/// On or off, as a pill with a knob.
pub(super) fn switch(
    id: &'static str,
    label: &'static str,
    on: bool,
    t: &Theme,
    on_pick: OnPick,
) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Switch)
        .aria_label(label)
        .aria_toggled(toggled(on))
        .relative()
        .flex_none()
        .w(t.size(30.0))
        .h(t.size(17.0))
        .rounded_full()
        .border_1()
        .cursor_pointer()
        .map(|d| {
            if on {
                d.bg(t.ai).border_color(t.ai)
            } else {
                d.bg(t.bg).border_color(t.line)
            }
        })
        .on_click(move |_, window, cx| on_pick(0, window, cx))
        .child(
            div()
                .absolute()
                .top(t.size(2.0))
                .left(t.size(if on { 15.0 } else { 2.0 }))
                .size(t.size(11.0))
                .rounded_full()
                .bg(if on { t.raised } else { t.faint }),
        )
}

/// What a stepper shows: its value, as a number and as read, and its bounds.
pub(super) struct Size {
    pub(super) value: f32,
    pub(super) text: String,
    pub(super) min: f32,
    pub(super) max: f32,
}

/// A size with a minus and a plus beside it: a spin button.
pub(super) fn stepper(
    id: &'static str,
    label: &'static str,
    size: Size,
    t: &Theme,
    mono: Option<SharedString>,
    on_pick: OnPick,
) -> Stateful<Div> {
    let Size {
        value,
        text,
        min,
        max,
    } = size;
    let hover = t.hover;
    let button = |part: usize, sign: &'static str, name: &'static str, enabled: bool| {
        let pick = Rc::clone(&on_pick);
        div()
            .id((id, part))
            .role(Role::Button)
            .aria_label(name)
            .flex()
            .items_center()
            .justify_center()
            .w(t.size(24.0))
            .text_color(if enabled { t.muted } else { t.faint })
            .when(enabled, |d| {
                d.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(move |_, window, cx| pick(part, window, cx))
            })
            .child(sign)
    };
    frame(t)
        .id(id)
        .role(Role::SpinButton)
        .aria_label(label)
        .aria_numeric_value(f64::from(value))
        .aria_min_numeric_value(f64::from(min))
        .aria_max_numeric_value(f64::from(max))
        .aria_numeric_value_step(1.0)
        .aria_value(text.clone())
        .flex()
        .items_stretch()
        .overflow_hidden()
        .child(button(0, "−", "Smaller", value > min))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .min_w(t.size(52.0))
                .border_l_1()
                .border_r_1()
                .border_color(t.line)
                .text_size(t.text(12.0))
                .when_some(mono, |d, family| d.font_family(family))
                .child(text),
        )
        .child(button(1, "+", "Larger", value < max))
}

/// A select's closed box: the chosen name and a chevron.
pub(super) fn select_box(
    id: &'static str,
    label: &'static str,
    shown: &'static str,
    open: bool,
    t: &Theme,
    on_pick: OnPick,
) -> Stateful<Div> {
    frame(t)
        .id(id)
        .role(Role::ComboBox)
        .aria_label(label)
        .aria_value(shown)
        .aria_expanded(open)
        .w(t.size(FIELD_WIDTH))
        .flex()
        .items_center()
        .gap(t.size(6.0))
        .pl(t.size(8.0))
        .pr(t.size(6.0))
        .text_size(t.text(12.5))
        .cursor_pointer()
        .on_click(move |_, window, cx| on_pick(0, window, cx))
        .child(div().flex_1().min_w_0().overflow_hidden().child(shown))
        .child(
            svg()
                .path(assets::CHEVRON)
                .size(t.size(11.0))
                .text_color(t.faint),
        )
}

/// A select's open list, under its box; `under_keyboard` is the choice the
/// arrows are on.
pub(super) fn select_list(
    id: &'static str,
    label: &'static str,
    choices: &[&'static str],
    chosen: usize,
    under_keyboard: usize,
    t: &Theme,
    on_pick: OnPick,
) -> Stateful<Div> {
    let hover = t.hover;
    let items = choices.iter().enumerate().map(|(at, name)| {
        let pick = Rc::clone(&on_pick);
        div()
            .id((id, at))
            .role(Role::ListBoxOption)
            .aria_label(*name)
            .aria_selected(at == chosen)
            .px(t.size(8.0))
            .py(t.size(3.0))
            .rounded(t.size(4.0))
            .text_size(t.text(12.5))
            .cursor_pointer()
            .when(at == under_keyboard, |d| d.bg(hover))
            .when(at == chosen, |d| d.font_weight(FontWeight::MEDIUM))
            .hover(move |s| s.bg(hover))
            .on_click(move |_, window, cx| pick(at, window, cx))
            .child(*name)
    });
    v_flex()
        .id(id)
        .role(Role::ListBox)
        .aria_label(label)
        .w(t.size(FIELD_WIDTH))
        .mt(t.size(4.0))
        .p(t.size(3.0))
        .border_1()
        .border_color(t.line)
        .rounded(t.size(5.0))
        .bg(t.bg)
        .children(items)
}

/// The frame a text field's input sits in.
pub(super) fn field_frame(focused: bool, t: &Theme) -> Div {
    frame(t)
        .w(t.size(FIELD_WIDTH))
        .flex()
        .items_center()
        .px(t.size(8.0))
        .overflow_hidden()
        .text_size(t.text(12.5))
        .when(focused, |d| d.border_color(t.focus_ring()))
}

/// The key a row's label carries, in small mono.
pub(super) fn key_label(key: &'static str, t: &Theme, mono: Option<SharedString>) -> Div {
    div()
        .flex_none()
        .text_size(t.text(11.0))
        .text_color(t.faint)
        .when_some(mono, |d, family| d.font_family(family))
        .child(key)
}

/// A row of the Keys section: the command's title with its area dimmed,
/// its id and its keys.
pub(super) fn key_row(
    title: &'static str,
    area_len: usize,
    id: &'static str,
    keys: Option<String>,
    odd: bool,
    t: &Theme,
    mono: Option<SharedString>,
) -> Div {
    let (area, rest) = title.split_at(area_len.min(title.len()));
    let stripe = Rgba {
        a: t.hover.a * 0.45,
        ..t.hover
    };
    h_flex()
        .items_center()
        .gap(t.size(10.0))
        .px(t.size(8.0))
        .py(t.size(5.0))
        .rounded(t.size(6.0))
        .when(odd, |d| d.bg(stripe))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(t.text(13.0))
                .child(div().flex_none().text_color(t.faint).child(area))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(rest),
                ),
        )
        .child(
            div()
                .flex_none()
                .w(t.size(150.0))
                .overflow_hidden()
                .text_size(t.text(11.0))
                .text_color(t.faint)
                .when_some(mono.clone(), |d, family| d.font_family(family))
                .child(id),
        )
        .child(
            div()
                .flex_none()
                .w(t.size(128.0))
                .flex()
                .justify_end()
                .text_size(t.text(12.0))
                .text_color(t.faint)
                .when_some(mono, |d, family| d.font_family(family))
                .child(keys.unwrap_or_else(|| "—".to_owned())),
        )
}
