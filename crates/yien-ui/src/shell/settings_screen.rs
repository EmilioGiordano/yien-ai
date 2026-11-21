//! The settings screen over the window (RF-CFG-05), option A of the canvas:
//! sections on the left with the find field and the file link, the rows of
//! one section on the right. Values come from the last valid reading of
//! `config.toml`; a change goes to the file through the host, and the
//! watcher's reading brings it back (RF-CFG-01). What a key or click means
//! is decided in `crate::settings_screen`.

use std::rc::Rc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tokio::sync::oneshot;
use yien_config::SettingValue;
use yien_proto::test_control::{SettingRowState, SettingsState};

use super::controls::{self, OnPick};
use super::{PALETTE_TOP, Shell};
use crate::actions::*;
use crate::assets;
use crate::catalog::{CATALOG, Scope, shortcut};
use crate::settings_screen::{
    Act, Control, ROWS, Row, Screen, Section, Stop, choice_index, row, size_label,
};

const DIALOG_WIDTH: f32 = 780.0;
const DIALOG_HEIGHT: f32 = 520.0;
const NAV_WIDTH: f32 = 188.0;
/// Window commands that leave the screen open: they act on what it shows.
const KEEP_OPEN: [&str; 3] = ["settings.open", "theme.toggle", "settings.open_config"];

/// The screen while it is open.
pub(super) struct SettingsDialog {
    model: Screen,
    /// The body, where the arrows move between rows.
    focus: FocusHandle,
    find: Entity<InputState>,
    /// One input per text row, by key.
    fields: Vec<(&'static str, Entity<InputState>)>,
    /// The text row whose input has the keyboard.
    editing: Option<&'static str>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsDialog {
    fn field(&self, key: &str) -> Option<&Entity<InputState>> {
        self.fields.iter().find(|(k, _)| *k == key).map(|(_, f)| f)
    }
}

/// A value as a text field or the test control shows it.
fn plain(value: &SettingValue) -> String {
    match value {
        SettingValue::Bool(b) => b.to_string(),
        SettingValue::Number(n) => n.to_string(),
        SettingValue::Text(s) => s.clone(),
    }
}

/// A value the test control sends for `key`, read as the row's control
/// would make it.
pub(super) fn value_for(key: &str, text: &str) -> Result<SettingValue, String> {
    let row = row(key).ok_or_else(|| format!("no setting {key}"))?;
    match row.control {
        Control::Switch => match text {
            "true" => Ok(SettingValue::Bool(true)),
            "false" => Ok(SettingValue::Bool(false)),
            _ => Err(format!("{key} takes true or false")),
        },
        Control::Stepper { .. } => text
            .trim()
            .parse::<f32>()
            .map(SettingValue::Number)
            .map_err(|_| format!("{key} takes a number")),
        Control::Segmented(_) | Control::Select(_) | Control::Text { .. } => {
            Ok(SettingValue::Text(text.to_owned()))
        }
    }
}

impl Shell {
    /// `settings: open settings` (RF-CFG-05): opens the screen with the
    /// keyboard in its find field, or closes it.
    pub(super) fn open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_screen.is_some() {
            self.close_settings(window, cx);
            return;
        }
        self.split_menu = None;
        self.confirm = None;
        self.close_palette(window, cx);
        self.close_tray(window, cx);
        self.finish_rename(true, window, cx);
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find a setting"));
        let mut subscriptions = vec![cx.subscribe_in(&find, window, Self::on_settings_find)];
        let model = Screen::default();
        let mut fields = Vec::new();
        for row in ROWS {
            let Control::Text { empty, .. } = row.control else {
                continue;
            };
            let shown = plain(&model.shown(row.key, self.settings.config()));
            let key = row.key;
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(empty)
                    .default_value(shown)
            });
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    this.on_settings_field(key, event, window, cx);
                },
            ));
            fields.push((key, input));
        }
        window.focus(&find.focus_handle(cx), cx);
        self.settings_screen = Some(SettingsDialog {
            model,
            focus: cx.focus_handle(),
            find,
            fields,
            editing: None,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Closes the screen; text typed in a field is kept, as leaving the
    /// field keeps it.
    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_settings_field(window, cx);
        if self.settings_screen.take().is_some() {
            self.give_back_keyboard(window, cx);
        }
    }

    fn on_settings_find(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let query = input.read(cx).value();
        if let Some(dialog) = self.settings_screen.as_mut() {
            dialog.model.set_query(&query);
            dialog.scroll.scroll_to_item(0);
            cx.notify();
        }
    }

    /// A text field took or left the keyboard: leaving keeps what was
    /// typed, as Enter does.
    fn on_settings_field(
        &mut self,
        key: &'static str,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.settings_screen.as_mut() else {
            return;
        };
        match event {
            InputEvent::Focus => {
                dialog.editing = Some(key);
                dialog.model.point_at(key);
                cx.notify();
            }
            InputEvent::Blur if dialog.editing == Some(key) => {
                self.commit_settings_field(window, cx);
            }
            _ => {}
        }
    }

    /// Writes the text field being edited, if its text changed, and ends
    /// the edit.
    fn commit_settings_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.settings_screen.as_mut() else {
            return;
        };
        let Some(key) = dialog.editing.take() else {
            return;
        };
        let Some(input) = dialog.field(key).cloned() else {
            return;
        };
        let typed = input.read(cx).value().trim().to_owned();
        let shown = dialog.model.shown(key, self.settings.config());
        if SettingValue::Text(typed.clone()) != shown {
            self.write_setting(key, SettingValue::Text(typed), window, cx);
        }
    }

    /// Ends the edit of a text field without writing, its text back to
    /// the value shown.
    fn revert_settings_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.settings.config().clone();
        let Some(dialog) = self.settings_screen.as_mut() else {
            return;
        };
        let Some(key) = dialog.editing.take() else {
            return;
        };
        let shown = plain(&dialog.model.shown(key, &config));
        if let Some(input) = dialog.field(key).cloned() {
            input.update(cx, |state, cx| state.set_value(shown, window, cx));
        }
    }

    /// Sends one value to the file, refusing while the file is invalid:
    /// the screen never writes over a file it could not read.
    pub(super) fn write_setting(
        &mut self,
        key: &'static str,
        value: SettingValue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(problem) = self.settings.problem() {
            let text = format!("{problem} Fix it in the file first; nothing was changed.");
            self.fail(text, cx);
            self.refresh_settings_fields(window, cx);
            return;
        }
        let value = match yien_config::check_value(key, &value) {
            Ok(value) => value,
            Err(error) => {
                self.fail(format!("{key} was not changed: {error}."), cx);
                self.refresh_settings_fields(window, cx);
                return;
            }
        };
        if let Some(dialog) = self.settings_screen.as_mut() {
            dialog.model.written(key, value.clone());
        }
        let (done, outcome) = oneshot::channel();
        self.settings_host.set_setting(
            key,
            value,
            Box::new(move |error| {
                let _ = done.send(error);
            }),
        );
        cx.spawn_in(window, async move |this, cx| {
            let error = match outcome.await {
                Ok(error) => error,
                Err(_) => Some("The settings file was not written.".to_owned()),
            };
            if let Some(error) = error {
                let _ = this.update_in(cx, |shell, window, cx| {
                    if let Some(dialog) = shell.settings_screen.as_mut() {
                        dialog.model.write_failed(key);
                    }
                    shell.fail(error, cx);
                    shell.refresh_settings_fields(window, cx);
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// The file was read again: the screen shows what it says now, and the
    /// text fields not being edited take its values.
    pub(super) fn settings_screen_read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = self.settings_screen.as_mut() {
            dialog.model.file_read();
        }
        self.refresh_settings_fields(window, cx);
    }

    fn refresh_settings_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.settings.config().clone();
        let Some(dialog) = self.settings_screen.as_ref() else {
            return;
        };
        for (key, input) in &dialog.fields {
            if dialog.editing == Some(*key) {
                continue;
            }
            let shown = plain(&dialog.model.shown(key, &config));
            if input.read(cx).value().as_ref() != shown.as_str() {
                input.update(cx, |state, cx| state.set_value(shown, window, cx));
            }
        }
        cx.notify();
    }

    /// Does what the model asked for.
    fn settings_act(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            Act::None => {}
            Act::Write(key, value) => self.write_setting(key, value, window, cx),
            Act::EditText(key) => {
                let field = self
                    .settings_screen
                    .as_ref()
                    .and_then(|d| d.field(key).cloned());
                if let Some(input) = field {
                    input.update(cx, |state, cx| state.select_all(window, cx));
                    window.focus(&input.focus_handle(cx), cx);
                }
            }
            Act::OpenFile => self.open_config_file(&OpenConfigFile, window, cx),
        }
        cx.notify();
    }

    /// Gives the keyboard to the body, or to the find field when the
    /// model's cursor left the body, and brings the cursor into view.
    fn settings_follow_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.settings_screen.as_ref() else {
            return;
        };
        match dialog.model.cursor() {
            Some(at) => {
                window.focus(&dialog.focus, cx);
                dialog.scroll.scroll_to_item(at);
            }
            None => {
                window.focus(&dialog.find.focus_handle(cx), cx);
                dialog.scroll.scroll_to_item(0);
            }
        }
        cx.notify();
    }

    fn settings_editing(&self) -> bool {
        self.settings_screen
            .as_ref()
            .is_some_and(|d| d.editing.is_some())
    }

    fn settings_up(&mut self, _: &SettingsUp, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_settings_field(window, cx);
        if let Some(dialog) = self.settings_screen.as_mut() {
            dialog.model.up();
        }
        self.settings_follow_cursor(window, cx);
    }

    fn settings_down(&mut self, _: &SettingsDown, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_settings_field(window, cx);
        if let Some(dialog) = self.settings_screen.as_mut() {
            dialog.model.down();
        }
        self.settings_follow_cursor(window, cx);
    }

    fn settings_side(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.settings.config().clone();
        let act = match self.settings_screen.as_ref() {
            Some(dialog) => dialog.model.step(&config, forward),
            None => return,
        };
        self.settings_act(act, window, cx);
    }

    fn settings_left(&mut self, _: &SettingsLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_side(false, window, cx);
    }

    fn settings_right(&mut self, _: &SettingsRight, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_side(true, window, cx);
    }

    /// Enter and Space: in a text field, keeps the text; in the find field,
    /// goes to the first row; on a row, flips, picks or opens.
    fn settings_activate(
        &mut self,
        _: &SettingsActivate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_editing() {
            self.commit_settings_field(window, cx);
            self.settings_follow_cursor(window, cx);
            return;
        }
        let config = self.settings.config().clone();
        let Some(dialog) = self.settings_screen.as_mut() else {
            return;
        };
        if dialog.model.cursor().is_none() {
            dialog.model.down();
            self.settings_follow_cursor(window, cx);
            return;
        }
        let act = dialog.model.activate(&config);
        self.settings_act(act, window, cx);
    }

    /// Escape: leaves a text field unchanged, closes an open list, and
    /// otherwise closes the screen.
    fn settings_close(&mut self, _: &SettingsClose, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_editing() {
            self.revert_settings_field(window, cx);
            self.settings_follow_cursor(window, cx);
            return;
        }
        let closed_list = self
            .settings_screen
            .as_mut()
            .is_some_and(|d| d.model.close_select());
        if closed_list {
            cx.notify();
        } else {
            self.close_settings(window, cx);
        }
    }

    fn settings_section_step(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_settings_field(window, cx);
        if let Some(dialog) = self.settings_screen.as_mut() {
            dialog.model.next_section(forward);
            if dialog.model.cursor().is_none() {
                dialog
                    .find
                    .update(cx, |state, cx| state.set_value("", window, cx));
            }
        }
        self.settings_follow_cursor(window, cx);
    }

    fn settings_next_section(
        &mut self,
        _: &SettingsNextSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_section_step(true, window, cx);
    }

    fn settings_previous_section(
        &mut self,
        _: &SettingsPreviousSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_section_step(false, window, cx);
    }

    /// A click on a section in the list.
    fn settings_show_section(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_settings_field(window, cx);
        if let Some(dialog) = self.settings_screen.as_mut() {
            dialog.model.show_section(section);
            dialog
                .find
                .update(cx, |state, cx| state.set_value("", window, cx));
            dialog.scroll.scroll_to_item(0);
        }
        cx.notify();
    }

    /// A click on a row, outside its control: the keyboard goes there.
    fn settings_point(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.settings_screen.as_mut() else {
            return;
        };
        dialog.model.point_at(key);
        let text = matches!(row(key).map(|r| r.control), Some(Control::Text { .. }));
        if !text {
            self.commit_settings_field(window, cx);
            self.settings_follow_cursor(window, cx);
        }
    }

    /// A click on a control's part: a choice, a stepper's minus or plus,
    /// a switch, a select's box or one of its choices.
    fn settings_pick(
        &mut self,
        key: &'static str,
        part: usize,
        list_choice: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_settings_field(window, cx);
        let config = self.settings.config().clone();
        let Some(dialog) = self.settings_screen.as_mut() else {
            return;
        };
        let act = match row(key).map(|r| r.control) {
            Some(Control::Segmented(list)) => match list.get(part) {
                Some(choice) => dialog.model.pick(key, choice.value, &config),
                None => Act::None,
            },
            Some(Control::Select(list)) if list_choice => match list.get(part) {
                Some(choice) => dialog.model.pick(key, choice.value, &config),
                None => Act::None,
            },
            Some(Control::Select(_)) => {
                dialog.model.click_select(key, &config);
                Act::None
            }
            Some(Control::Stepper { .. }) => dialog.model.click_step(key, part == 1, &config),
            Some(Control::Switch) => dialog.model.click_switch(key, &config),
            Some(Control::Text { .. }) | None => Act::None,
        };
        self.settings_follow_cursor(window, cx);
        self.settings_act(act, window, cx);
    }

    /// A click handler for `key`'s control that reaches [`Self::settings_pick`].
    fn settings_picker(&self, key: &'static str, list: bool, cx: &mut Context<Self>) -> OnPick {
        let shell = cx.entity().downgrade();
        Rc::new(move |part, window, cx| {
            cx.stop_propagation();
            let _ = shell.update(cx, |shell, cx| {
                shell.settings_pick(key, part, list, window, cx);
            });
        })
    }

    /// While the screen is open, a window command closes it and goes no
    /// further, so nothing acts on the workspace hidden behind it; the few
    /// that act on what it shows keep it open.
    fn close_settings_on_window_commands(
        &self,
        dialog: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        CATALOG
            .iter()
            .filter(|spec| spec.scope == Scope::Window && !KEEP_OPEN.contains(&spec.id))
            .fold(dialog, |dialog, spec| {
                dialog.on_boxed_action(
                    &*(spec.action)(),
                    cx.listener(|this, _: &dyn Action, window, cx| {
                        this.close_settings(window, cx);
                    }),
                )
            })
    }

    /// The screen's state for the test control.
    pub(super) fn settings_test_state(&self, window: &Window) -> Option<SettingsState> {
        let dialog = self.settings_screen.as_ref()?;
        let config = self.settings.config();
        let cursor = dialog.model.cursor_stop().map(|stop| match stop {
            Stop::Row(row) => row.key.to_owned(),
            Stop::EditKeys => "edit_keys".to_owned(),
            Stop::Command(at) => CATALOG.get(at).map_or("", |s| s.id).to_owned(),
        });
        Some(SettingsState {
            section: dialog.model.section().name().to_owned(),
            query: dialog.model.query().to_owned(),
            cursor,
            body_has_keyboard: dialog.focus.is_focused(window),
            editing: dialog.editing.map(str::to_owned),
            open_select: dialog.model.open_select().map(|(key, _)| key.to_owned()),
            problem: self.settings.problem().map(str::to_owned),
            rows: dialog
                .model
                .stops()
                .iter()
                .filter_map(|stop| match stop {
                    Stop::Row(row) => Some(SettingRowState {
                        key: row.key.to_owned(),
                        value: plain(&dialog.model.shown(row.key, config)),
                    }),
                    _ => None,
                })
                .collect(),
        })
    }

    /// The test control's `settings.set`: the value goes the way a control
    /// sends it; the keyboard stays where it is.
    pub(super) fn test_set_setting(
        &mut self,
        key: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.settings_screen.is_none() {
            return Err("the settings screen is not open; run settings.open first".to_owned());
        }
        let row = row(key).ok_or_else(|| format!("no setting {key}"))?;
        let value = value_for(key, value)?;
        self.write_setting(row.key, value, window, cx);
        Ok(())
    }

    pub(super) fn render_settings(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let dialog = self.settings_screen.as_ref()?;
        let t = self.theme;
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .justify_center()
                .items_start()
                .bg(rgba(0x0000_0059))
                .child(
                    h_flex()
                        .id("settings")
                        .role(Role::Dialog)
                        .aria_label("Settings")
                        .track_focus(&dialog.focus)
                        .key_context(SETTINGS_CONTEXT)
                        .mt(t.size(PALETTE_TOP))
                        .w(t.size(DIALOG_WIDTH))
                        .h(t.size(DIALOG_HEIGHT))
                        .max_w(relative(0.9))
                        .max_h(relative(0.85))
                        .items_stretch()
                        .overflow_hidden()
                        .rounded(t.size(10.0))
                        .border_1()
                        .border_color(t.line)
                        .bg(t.raised)
                        .shadow_lg()
                        .on_action(cx.listener(Self::settings_up))
                        .on_action(cx.listener(Self::settings_down))
                        .on_action(cx.listener(Self::settings_left))
                        .on_action(cx.listener(Self::settings_right))
                        .on_action(cx.listener(Self::settings_activate))
                        .on_action(cx.listener(Self::settings_close))
                        .on_action(cx.listener(Self::settings_next_section))
                        .on_action(cx.listener(Self::settings_previous_section))
                        .map(|d| self.close_settings_on_window_commands(d, cx))
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.close_settings(window, cx);
                        }))
                        .child(self.render_settings_nav(dialog, cx))
                        .child(self.render_settings_body(dialog, window, cx)),
                ),
        )
    }

    fn render_settings_nav(&self, dialog: &SettingsDialog, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let hover = t.hover;
        let current = (!dialog.model.finding()).then(|| dialog.model.section());
        let items = Section::ALL.iter().map(|section| {
            let section = *section;
            let on = current == Some(section);
            div()
                .id(("settings-section", section as usize))
                .role(Role::Tab)
                .aria_label(section.name())
                .aria_selected(on)
                .px(t.size(8.0))
                .py(t.size(5.0))
                .rounded(t.size(6.0))
                .border_1()
                .text_size(t.text(13.0))
                .cursor_pointer()
                .map(|d| {
                    if on {
                        d.bg(t.raised)
                            .border_color(t.line)
                            .text_color(t.text)
                            .font_weight(FontWeight::MEDIUM)
                    } else {
                        d.border_color(transparent_black())
                            .text_color(t.muted)
                            .hover(move |s| s.bg(hover))
                    }
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.settings_show_section(section, window, cx);
                }))
                .child(section.name())
        });
        v_flex()
            .flex_none()
            .w(t.size(NAV_WIDTH))
            .gap(t.size(2.0))
            .px(t.size(8.0))
            .py(t.size(10.0))
            .border_r_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(
                div()
                    .px(t.size(8.0))
                    .pt(t.size(2.0))
                    .pb(t.size(6.0))
                    .text_size(t.text(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.faint)
                    .child("Settings"),
            )
            .child(
                div()
                    .mb(t.size(8.0))
                    .px(t.size(8.0))
                    .py(t.size(4.0))
                    .border_1()
                    .border_color(t.line)
                    .rounded(t.size(5.0))
                    .bg(t.bg)
                    .text_size(t.text(12.5))
                    .child(
                        Input::new(&dialog.find)
                            .appearance(false)
                            .aria_label("Find a setting"),
                    ),
            )
            .child(
                v_flex()
                    .id("settings-sections")
                    .role(Role::TabList)
                    .aria_label("Settings sections")
                    .gap(t.size(2.0))
                    .children(items),
            )
            .child(div().flex_1())
            .child(
                v_flex()
                    .id("settings-open-file")
                    .role(Role::Button)
                    .aria_label("Open settings file config.toml")
                    .gap(t.size(1.0))
                    .px(t.size(8.0))
                    .py(t.size(5.0))
                    .rounded(t.size(6.0))
                    .whitespace_nowrap()
                    .text_size(t.text(12.5))
                    .text_color(t.muted)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_config_file(&OpenConfigFile, window, cx);
                    }))
                    .child("Open settings file")
                    .child(
                        div()
                            .text_size(t.text(11.0))
                            .text_color(t.faint)
                            .when_some(self.fonts.mono.clone(), |d, f| d.font_family(f))
                            .child(yien_config::FILE_NAME),
                    ),
            )
    }

    fn render_settings_body(
        &self,
        dialog: &SettingsDialog,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = self.theme;
        let hover = t.hover;
        let model = &dialog.model;
        let title = if model.finding() {
            "Results".to_owned()
        } else {
            model.section().name().to_owned()
        };
        let stops = model.stops();
        let body_keyboard = dialog.focus.is_focused(window);
        let keymap = cx.key_bindings();
        let elements: Vec<AnyElement> = stops
            .iter()
            .enumerate()
            .map(|(at, stop)| {
                let under = model.cursor() == Some(at) && body_keyboard;
                match stop {
                    Stop::Row(row) => self.render_settings_row(row, at, under, dialog, cx),
                    Stop::EditKeys => self.render_keys_note(under, cx),
                    Stop::Command(index) => {
                        let Some(spec) = CATALOG.get(*index) else {
                            return div().into_any_element();
                        };
                        let keys = shortcut(&keymap.borrow(), (spec.action)().as_ref());
                        controls::key_row(
                            spec.title,
                            spec.area_len(),
                            spec.id,
                            keys,
                            index % 2 == 0,
                            &t,
                            self.fonts.mono.clone(),
                        )
                        .id(("settings-key", *index))
                        .role(Role::ListItem)
                        .when(under, |d| d.border_1().border_color(t.focus_ring()))
                        .into_any_element()
                    }
                }
            })
            .collect();
        let empty = stops.is_empty().then(|| {
            div()
                .pt(t.size(10.0))
                .text_size(t.text(13.0))
                .text_color(t.muted)
                .child(format!("No setting matches \"{}\".", model.query().trim()))
        });
        let problem = self.settings.problem().map(|problem| {
            div()
                .id("settings-problem")
                .role(Role::Alert)
                .mt(t.size(6.0))
                .px(t.size(10.0))
                .py(t.size(6.0))
                .rounded(t.size(6.0))
                .border_1()
                .border_color(t.ochre)
                .text_size(t.text(12.0))
                .text_color(t.ochre)
                .child(format!(
                    "{problem} Until it is fixed, the screen shows the last valid settings \
                     and changes nothing."
                ))
        });
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .px(t.size(22.0))
            .pt(t.size(14.0))
            .pb(t.size(18.0))
            .child(
                h_flex()
                    .items_center()
                    .mb(t.size(4.0))
                    .child(
                        div()
                            .id("settings-title")
                            .flex_1()
                            .role(Role::Heading)
                            .text_size(t.text(15.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .id("settings-close")
                            .role(Role::Button)
                            .aria_label("Close settings (Esc)")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(t.size(20.0))
                            .rounded(t.size(4.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_settings(window, cx);
                            }))
                            .child(
                                svg()
                                    .path(assets::CLOSE)
                                    .size(t.size(13.0))
                                    .text_color(t.muted),
                            ),
                    ),
            )
            .children(problem)
            .child(
                v_flex()
                    .id("settings-rows")
                    .role(if model.section() == Section::Keys && !model.finding() {
                        Role::List
                    } else {
                        Role::Group
                    })
                    .aria_label(if model.finding() {
                        "Results".to_owned()
                    } else {
                        model.section().name().to_owned()
                    })
                    .flex_1()
                    .min_h_0()
                    .track_scroll(&dialog.scroll)
                    .overflow_y_scroll()
                    .when(self.settings.problem().is_some(), |d| d.opacity(0.55))
                    .children(elements)
                    .children(empty),
            )
    }

    fn render_keys_note(&self, under: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        h_flex()
            .items_center()
            .gap(t.size(8.0))
            .mt(t.size(4.0))
            .mb(t.size(8.0))
            .text_size(t.text(12.0))
            .text_color(t.faint)
            .child("Read only.")
            .child(
                div()
                    .id("settings-edit-keys")
                    .role(Role::Link)
                    .aria_label("Edit keys in config.toml")
                    .text_color(t.ai)
                    .cursor_pointer()
                    .when(under, |d| d.border_b_1().border_color(t.focus_ring()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_config_file(&OpenConfigFile, window, cx);
                    }))
                    .child("Edit in config.toml"),
            )
            .into_any_element()
    }

    fn render_settings_row(
        &self,
        row: &'static Row,
        at: usize,
        under: bool,
        dialog: &SettingsDialog,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let mono = self.fonts.mono.clone();
        let key = row.key;
        let shown = dialog.model.shown(key, self.settings.config());
        let separator = Rgba {
            a: t.line.a * 0.55,
            ..t.line
        };
        let control: AnyElement = match row.control {
            Control::Segmented(list) => {
                let names: Vec<&'static str> = list.iter().map(|c| c.label).collect();
                controls::segmented(
                    key,
                    row.label,
                    &names,
                    choice_index(list, &shown),
                    &t,
                    self.settings_picker(key, false, cx),
                )
                .into_any_element()
            }
            Control::Select(list) => {
                let chosen = choice_index(list, &shown);
                let open = dialog.model.open_select().filter(|(k, _)| *k == key);
                let names: Vec<&'static str> = list.iter().map(|c| c.label).collect();
                v_flex()
                    .items_end()
                    .child(controls::select_box(
                        key,
                        row.label,
                        list[chosen].label,
                        open.is_some(),
                        &t,
                        self.settings_picker(key, false, cx),
                    ))
                    .when_some(open, |d, (_, under_keyboard)| {
                        d.child(controls::select_list(
                            "settings-select-list",
                            row.label,
                            &names,
                            chosen,
                            under_keyboard,
                            &t,
                            self.settings_picker(key, true, cx),
                        ))
                    })
                    .into_any_element()
            }
            Control::Stepper { min, max } => {
                let value = match shown {
                    SettingValue::Number(n) => n,
                    _ => min,
                };
                controls::stepper(
                    key,
                    row.label,
                    value,
                    size_label(&shown),
                    (min, max),
                    &t,
                    mono.clone(),
                    self.settings_picker(key, false, cx),
                )
                .into_any_element()
            }
            Control::Switch => controls::switch(
                key,
                row.label,
                shown == SettingValue::Bool(true),
                &t,
                self.settings_picker(key, false, cx),
            )
            .into_any_element(),
            Control::Text {
                mono: monospaced, ..
            } => match dialog.field(key) {
                Some(input) => controls::field_frame(dialog.editing == Some(key), &t)
                    .when(monospaced, |d| {
                        d.text_size(t.text(12.0))
                            .when_some(mono.clone(), |d, f| d.font_family(f))
                    })
                    .child(Input::new(input).appearance(false).aria_label(row.label))
                    .into_any_element(),
                None => div().into_any_element(),
            },
        };
        let hover = t.hover;
        h_flex()
            .id(("settings-row", at))
            .role(Role::Group)
            .aria_label(row.label)
            .aria_description(row.doc)
            .items_center()
            .gap(t.size(16.0))
            .py(t.size(9.0))
            .mx(t.size(-8.0))
            .px(t.size(8.0))
            .rounded(t.size(6.0))
            .when(at > 0, |d| d.border_t_1().border_color(separator))
            .when(under, |d| d.bg(hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.settings_point(key, window, cx);
            }))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .items_baseline()
                            .gap(t.size(8.0))
                            .text_size(t.text(13.0))
                            .child(row.label)
                            .child(controls::key_label(key, &t, mono)),
                    )
                    .child(
                        div()
                            .mt(t.size(1.0))
                            .text_size(t.text(12.0))
                            .text_color(t.faint)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(row.doc),
                    ),
            )
            .child(div().flex_none().flex().items_center().child(control))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use yien_config::SettingValue;

    use super::{KEEP_OPEN, plain, value_for};
    use crate::catalog::CATALOG;

    #[test]
    fn rf_cfg_05_the_test_control_sends_values_as_the_controls_do() {
        assert_eq!(
            value_for("ui.font.size", "17"),
            Ok(SettingValue::Number(17.0))
        );
        assert_eq!(
            value_for("update.check", "false"),
            Ok(SettingValue::Bool(false))
        );
        assert_eq!(
            value_for("theme", "washi"),
            Ok(SettingValue::Text("washi".into()))
        );
        assert!(value_for("update.check", "no").is_err());
        assert!(value_for("ui.font.size", "big").is_err());
        assert!(value_for("colour", "red").is_err());
    }

    #[test]
    fn rf_cfg_05_values_read_back_as_typed() {
        assert_eq!(plain(&SettingValue::Number(17.0)), "17");
        assert_eq!(plain(&SettingValue::Number(14.5)), "14.5");
        assert_eq!(plain(&SettingValue::Bool(true)), "true");
        assert_eq!(plain(&SettingValue::Text("D:\\Code".into())), "D:\\Code");
    }

    #[test]
    fn rf_cfg_05_only_commands_on_what_it_shows_keep_the_screen_open() {
        for id in KEEP_OPEN {
            assert!(CATALOG.iter().any(|s| s.id == id), "{id}");
        }
    }
}
