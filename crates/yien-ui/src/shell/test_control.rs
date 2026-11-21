//! Answers the test control's requests (ADR-21) on the window's thread,
//! through the paths the user's own input takes: commands as the palette
//! runs them, text as the input handler delivers it, keys dispatched as if
//! pressed.

use std::time::Instant;

use gpui_kit::*;
use yien_proto::PaneId;
use yien_proto::test_control::{Reply, Request, TEST_CONTROL_VERSION, UiState, WindowState};

use super::{STATUS_BAR_HEIGHT, Shell, TITLE_BAR_HEIGHT};
use crate::catalog::spec;
use crate::region::Region;
use crate::test_control::{
    Run, TestControl, expired_reply, layout_state, parse_key, resolve_command, workspace_state,
};

impl Shell {
    /// Answers each request in turn until the pipe's sender goes away.
    pub(super) fn listen_to_test_control(
        control: TestControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut calls = control.calls;
        cx.spawn_in(window, async move |this, cx| {
            while let Some(call) = calls.recv().await {
                if call.expired(Instant::now()) {
                    let _ = call.reply.send(expired_reply());
                    continue;
                }
                let reply = match call.request {
                    // Dispatched outside the shell's update: the key may run
                    // one of the shell's own actions, which updates it again.
                    Request::Key { pane, key } => {
                        let keystroke = match parse_key(&key) {
                            Ok(keystroke) => keystroke,
                            Err(error) => {
                                let _ = call.reply.send(Reply::error(error));
                                continue;
                            }
                        };
                        let target = this.update_in(cx, |shell, window, cx| match pane {
                            Some(pane) => shell.keyboard_pane_is(pane, window, cx).map(|_| ()),
                            None => Ok(()),
                        });
                        match target {
                            Ok(Ok(())) => {
                                match cx
                                    .update(|window, cx| window.dispatch_keystroke(keystroke, cx))
                                {
                                    Ok(handled) => Reply::key(handled),
                                    Err(_) => return,
                                }
                            }
                            Ok(Err(error)) => Reply::error(error),
                            Err(_) => return,
                        }
                    }
                    // Dispatched outside the shell's update, like a key: the
                    // click runs the shell's own listeners.
                    Request::Click { x, y } => {
                        let at = point(px(x), px(y));
                        match cx.update(|window, cx| click(window, at, cx)) {
                            Ok(()) => Reply::done(),
                            Err(_) => return,
                        }
                    }
                    request => {
                        match this.update_in(cx, |shell, window, cx| {
                            shell.answer_test(request, window, cx)
                        }) {
                            Ok(reply) => reply,
                            Err(_) => return,
                        }
                    }
                };
                let _ = call.reply.send(reply);
            }
        })
        .detach();
    }

    fn answer_test(
        &mut self,
        request: Request,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Reply {
        match request {
            Request::State { text } => Reply::state(self.test_state(text, window, cx)),
            Request::Command { id, args } => match resolve_command(&id, &args, &self.workspace) {
                Ok(Run::Catalog(id)) => {
                    if let Some(spec) = spec(id) {
                        window.dispatch_action((spec.action)(), cx);
                    }
                    Reply::done()
                }
                Ok(Run::Target(target)) => {
                    self.run_target(target, window, cx);
                    Reply::done()
                }
                Ok(Run::FilesMenu { path }) => {
                    let path = (path != ".").then_some(path);
                    match self.test_open_file_menu(path, window, cx) {
                        Ok(()) => Reply::done(),
                        Err(error) => Reply::error(error),
                    }
                }
                Ok(Run::FilesMenuPick { item, name }) => {
                    match self.test_pick_file_menu(&item, name, window, cx) {
                        Ok(()) => Reply::done(),
                        Err(error) => Reply::error(error),
                    }
                }
                Err(error) => Reply::error(error),
            },
            Request::Text { pane, text } => {
                let target = match pane {
                    Some(pane) => self.keyboard_pane_is(pane, window, cx),
                    None => self
                        .keyboard_pane_now(window, cx)
                        .ok_or_else(|| "no pane has the keyboard".to_owned()),
                };
                match target.map(|pane| self.terminals.get(&pane).cloned()) {
                    Ok(Some(terminal)) => {
                        terminal.update(cx, |terminal, cx| terminal.commit_text(text.as_str(), cx));
                        Reply::done()
                    }
                    Ok(None) => Reply::error("the pane has no terminal yet"),
                    Err(error) => Reply::error(error),
                }
            }
            Request::Resize { width, height } => {
                window.resize(size(px(width), px(height)));
                Reply::done()
            }
            Request::WindowClose {} => {
                // The X path hands the true to GPUI, which closes the window;
                // this path has no hook, so a real quit needs the removal.
                if self.window_close_requested(window, cx) {
                    window.remove_window();
                }
                Reply::done()
            }
            Request::Key { .. } | Request::Click { .. } => {
                Reply::error("keys and clicks are dispatched outside the shell")
            }
        }
    }

    /// The pane whose terminal or file preview holds the keyboard now, if one does.
    fn keyboard_pane_now(&self, window: &Window, cx: &App) -> Option<PaneId> {
        self.terminals
            .iter()
            .find(|(_, terminal)| terminal.read(cx).handle().is_focused(window))
            .map(|(pane, _)| *pane)
            .or_else(|| {
                self.file_panes
                    .iter()
                    .find(|(_, file_pane)| file_pane.read(cx).handle().is_focused(window))
                    .map(|(pane, _)| *pane)
            })
    }

    /// `pane`, when its terminal holds the keyboard: a user types only into
    /// that one, so a script must focus a pane first.
    fn keyboard_pane_is(&self, pane: PaneId, window: &Window, cx: &App) -> Result<PaneId, String> {
        match self.keyboard_pane_now(window, cx) {
            Some(now) if now == pane => Ok(pane),
            Some(now) => Err(format!(
                "pane {} does not have the keyboard, pane {} does; run pane.focus first",
                pane.get(),
                now.get()
            )),
            None => Err(format!(
                "pane {} does not have the keyboard, no pane does",
                pane.get()
            )),
        }
    }

    fn test_state(&self, with_text: bool, window: &Window, cx: &App) -> UiState {
        let keyboard = self.keyboard_pane_now(window, cx);
        let terminal = |pane: PaneId| {
            self.terminals
                .get(&pane)
                .and_then(|t| t.read(cx).facts(with_text))
                .or_else(|| {
                    self.file_panes
                        .get(&pane)
                        .and_then(|f| f.read(cx).facts(with_text))
                })
        };
        let (projects, tabs, panes) = workspace_state(&self.workspace, keyboard, &terminal);
        let list = self.tab_drag.scroll.bounds();
        let painted = |tab| {
            self.tab_menu.shown_row(tab, list).map(|b| {
                [
                    b.origin.x.as_f32(),
                    b.origin.y.as_f32(),
                    b.size.width.as_f32(),
                    b.size.height.as_f32(),
                ]
            })
        };
        let layout = layout_state(
            &self.theme,
            (TITLE_BAR_HEIGHT, STATUS_BAR_HEIGHT),
            self.bar_widths,
            &tabs,
            &painted,
        );
        let viewport = window.viewport_size();
        UiState {
            version: TEST_CONTROL_VERSION,
            window: WindowState {
                width: viewport.width.as_f32(),
                height: viewport.height.as_f32(),
                active: window.is_window_active(),
                window_visible: self.os_window.visible(),
                region: region_name(self.region(window, cx)).to_owned(),
                overlay: self.overlay().map(str::to_owned),
            },
            keyboard_pane: keyboard,
            selected_tab: self.workspace.selected_tab().map(|t| t.id),
            waiting: self.workspace.needs_you_panes(),
            projects,
            tabs,
            panes,
            files: self.files_test_state(cx),
            layout,
        }
    }
}

/// A left click at `at` as the pointer makes it: a move there, so hover
/// and hit-testing see it, then a press and a release.
fn click(window: &mut Window, at: Point<Pixels>, cx: &mut App) {
    let modifiers = Modifiers::default();
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position: at,
            pressed_button: None,
            modifiers,
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            modifiers,
            click_count: 1,
        }),
        cx,
    );
}

fn region_name(region: Region) -> &'static str {
    match region {
        Region::TabBar => "tab_bar",
        Region::Panes => "panes",
        Region::Dividers => "dividers",
        Region::ContextBar => "context_bar",
    }
}
