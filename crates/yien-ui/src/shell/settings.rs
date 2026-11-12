//! Applying the settings file to the window (RF-CFG-01, RF-CFG-02): theme,
//! fonts and sizes change live, terminals included, and a bad file leaves a
//! notice. What a reading means is decided in `crate::settings`.

use gpui_kit::*;
use tokio::sync::{mpsc, oneshot};
use yien_proto::{ClientMsg, TerminalPalette};

use super::Shell;
use crate::actions::{self, OpenConfigFile, ToggleTheme};
use crate::bridge::{Sent, deliver};
use crate::fonts::Fonts;
use crate::keys;
use crate::settings::{ConfigUpdate, Notice, after_reading};
use crate::theme::{Theme, sync_component_theme};

impl Shell {
    /// Applies every reading the binary sends, as it arrives.
    pub(super) fn listen_to_settings(
        mut updates: mpsc::UnboundedReceiver<ConfigUpdate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(update) = updates.recv().await {
                let applied = this.update_in(cx, |shell, window, cx| {
                    shell.apply_config(update, window, cx)
                });
                if applied.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Takes a reading: a valid one replaces the settings and redraws with
    /// them; any problem goes to the status bar.
    pub(super) fn apply_config(
        &mut self,
        update: ConfigUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &update {
            ConfigUpdate::Loaded(loaded) => {
                tracing::info!(warnings = loaded.warnings.len(), "settings applied");
            }
            ConfigUpdate::Invalid(p) => tracing::warn!(
                line = p.line,
                column = p.column,
                "the settings file is invalid; the last valid settings stay"
            ),
            ConfigUpdate::Unreadable(why) => {
                tracing::warn!("the settings file could not be read: {why}");
            }
        }
        let loaded = matches!(update, ConfigUpdate::Loaded(_));
        let mut notices = Vec::new();
        if let Notice::Show(text) = self.settings.apply(update) {
            notices.push(text);
        }
        // A reading is when a newly installed font can be picked up.
        self.installed_fonts = cx.text_system().all_font_names();
        notices.extend(self.apply_look(window, cx));
        if loaded {
            notices.extend(self.apply_keys(cx));
        }
        let grouped = self.settings.groups_by_state();
        if grouped != self.workspace.groups_by_state() {
            self.workspace.set_group_by_state(grouped);
            self.tab_cursor = None;
        }
        let before = self.notice.as_ref().map(|n| n.text.clone());
        let (text, settings_notice) = after_reading(
            before.as_deref(),
            self.settings_notice.as_deref(),
            self.profiles_notice.as_deref(),
            &notices,
        );
        self.settings_notice = settings_notice;
        if text != before {
            match text {
                Some(text) => self.show(
                    super::status::Notice::new(text, super::status::reading_kind(loaded)),
                    cx,
                ),
                None => {
                    self.clear_notice(cx);
                }
            }
        }
        cx.notify();
    }

    /// Redraws with the current settings, the OS appearance and the theme
    /// toggle, with the font list of the last reading. Returns notices for
    /// families that are not installed.
    pub(super) fn apply_look(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<String> {
        let config = self.settings.config().clone();
        let kind = self.settings.theme(system_dark(window));
        let theme = Theme::of(kind).with_ui_size(config.ui_font.size);
        let (fonts, notices) = Fonts::configured(
            &self.installed_fonts,
            config.ui_font.family.as_deref(),
            config.terminal_font.family.as_deref(),
        );
        if theme != self.theme {
            self.theme = theme;
            sync_component_theme(&theme, cx);
            for terminal in self.terminals.values() {
                terminal.update(cx, |t, cx| t.set_theme(theme, cx));
            }
        }
        let size = config.terminal_font.size;
        if fonts.terminal != self.fonts.terminal || size != self.terminal_size {
            self.terminal_size = size;
            for terminal in self.terminals.values() {
                let family = fonts.terminal.clone();
                terminal.update(cx, |t, cx| t.set_font(family, size, window, cx));
            }
        }
        self.fonts = fonts;
        self.sync_palette(cx);
        cx.notify();
        notices
    }

    /// Sends the theme's terminal palette until the core has taken it; a
    /// message refused by a full queue is sent again on the next call.
    pub(super) fn sync_palette(&mut self, cx: &mut Context<Self>) {
        let wanted = self.theme.kind.terminal_palette();
        if self.palette_sent == Some(wanted) {
            return;
        }
        let sent = deliver(
            &self.link,
            ClientMsg::SetTerminalPalette { palette: wanted },
            cx,
        );
        self.palette_sent = palette_taken(self.palette_sent, wanted, sent);
    }

    /// Rebuilds the keymap when the `[keys]` table changed what is bound:
    /// gpui-kit's keys first, then the catalog's with the rebinds.
    fn apply_keys(&mut self, cx: &mut Context<Self>) -> Vec<String> {
        let (rebinds, notices) = keys::plan(&self.settings.config().keys);
        if rebinds != self.rebinds {
            cx.clear_key_bindings();
            cx.bind_keys(self.base_keys.iter().cloned());
            cx.bind_keys(actions::bindings_with(&rebinds));
            tracing::info!(rebound = rebinds.len(), "key bindings rebuilt");
            self.rebinds = rebinds;
        }
        notices
    }

    pub(super) fn toggle_theme(
        &mut self,
        _: &ToggleTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.toggle_theme(system_dark(window));
        self.apply_look(window, cx);
    }

    pub(super) fn open_config_file(
        &mut self,
        _: &OpenConfigFile,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = self.settings.config().editor.clone();
        let (done, outcome) = oneshot::channel();
        self.settings_host.open_config(
            editor,
            Box::new(move |error| {
                let _ = done.send(error);
            }),
        );
        cx.spawn(async move |this, cx| {
            if let Ok(Some(error)) = outcome.await {
                let _ = this.update(cx, |shell, cx| {
                    shell.fail(error, cx);
                });
            }
        })
        .detach();
    }
}

fn system_dark(window: &Window) -> bool {
    matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// The palette the core holds after sending `wanted`: a queued message
/// reaches it in order, a refused one leaves what it had.
fn palette_taken(
    before: Option<TerminalPalette>,
    wanted: TerminalPalette,
    sent: Sent,
) -> Option<TerminalPalette> {
    match sent {
        Sent::Delivered | Sent::Queued => Some(wanted),
        Sent::Full | Sent::Stopped => before,
    }
}

#[cfg(test)]
mod tests {
    use super::{Sent, TerminalPalette, palette_taken};

    #[test]
    fn rf_cfg_02_a_refused_palette_is_sent_again() {
        let washi = TerminalPalette::Washi;
        assert_eq!(palette_taken(None, washi, Sent::Delivered), Some(washi));
        assert_eq!(palette_taken(None, washi, Sent::Queued), Some(washi));
        let sumi = Some(TerminalPalette::Sumi);
        assert_eq!(
            palette_taken(sumi, washi, Sent::Full),
            sumi,
            "still differs"
        );
        assert_eq!(palette_taken(None, washi, Sent::Stopped), None);
    }
}
