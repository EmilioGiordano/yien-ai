//! Zooming every terminal at once (RF-CFG-02). The model is `crate::zoom`;
//! this resizes the panes, says the new size and keeps the core told, so the
//! session saves the zoom.

use gpui_kit::*;
use yien_proto::{ClientMsg, Command};

use super::Shell;
use super::status::Notice;
use crate::actions::{ResetZoom, ZoomIn, ZoomOut};
use crate::zoom::{self, Zoom};

impl Shell {
    /// Takes the zoom the session saved, from the core's snapshot.
    pub(super) fn take_saved_zoom(
        &mut self,
        saved: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_zoom = zoom::sanitize(saved);
        self.terminal_zoom_saved = Some(saved);
        self.size_terminals(window, cx);
    }

    /// Gives every pane the zoomed size; each one sends its new grid size to
    /// the core, a pane on a hidden tab included.
    fn size_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let size = zoom::size(
            self.settings.config().terminal_font.size,
            self.terminal_zoom,
        );
        if size == self.terminal_size {
            return;
        }
        self.terminal_size = size;
        for terminal in self.terminals.values() {
            let family = self.fonts.terminal.clone();
            terminal.update(cx, |t, cx| t.set_font(family, size, window, cx));
        }
        cx.notify();
    }

    fn zoom(&mut self, step: Zoom, window: &mut Window, cx: &mut Context<Self>) {
        let base = self.settings.config().terminal_font.size;
        self.terminal_zoom = zoom::step(base, self.terminal_zoom, step);
        self.size_terminals(window, cx);
        self.save_zoom(cx);
        self.show(Notice::info(zoom::notice(self.terminal_size)), cx);
    }

    /// Tells the core the zoom once it differs from what it holds, and never
    /// before the core's snapshot brought the saved one, which the default
    /// would overwrite.
    fn save_zoom(&mut self, cx: &mut Context<Self>) {
        let Some(saved) = self.terminal_zoom_saved else {
            return;
        };
        if saved == self.terminal_zoom {
            return;
        }
        self.terminal_zoom_saved = Some(self.terminal_zoom);
        let zoom = self.terminal_zoom;
        self.send(ClientMsg::Command(Command::SetTerminalZoom { zoom }), cx);
    }

    pub(super) fn zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom(Zoom::In, window, cx);
    }

    pub(super) fn zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom(Zoom::Out, window, cx);
    }

    pub(super) fn reset_zoom(
        &mut self,
        _: &ResetZoom,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.zoom(Zoom::Reset, window, cx);
    }
}
