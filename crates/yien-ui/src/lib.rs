//! The YIEN window, drawn with GPUI: projects and their vertical tabs on the
//! left, the selected tab's terminal panes in the centre, the context bar on
//! the right.
//!
//! The UI never talks to the core directly (ADR-03). It sends `ClientMsg`
//! through a [`CoreClient`] the binary provides, and keeps a
//! [`WorkspaceView`] up to date from the `CoreMsg` events it receives.

#![forbid(unsafe_code)]

mod actions;
mod assets;
mod attention;
mod bridge;
mod catalog;
mod file_tree;
mod files;
mod fonts;
mod git;
mod grid;
mod key_quirks;
mod keys;
mod layout;
mod matcher;
mod model;
mod palette;
mod pane_head;
mod region;
mod release_notes;
mod settings;
mod shell;
mod sidebars;
mod tab_order;
mod terminal;
mod theme;
mod title_bar;
mod tray;
mod update;
mod worktree;

use std::rc::Rc;

use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext, KeyBinding, WindowBounds, WindowOptions, px, size};
use tokio::sync::mpsc;
use yien_proto::CoreMsg;

pub use attention::{OsAttention, ToastText};
pub use bridge::{CoreClient, Refused};
pub use files::{Files, FilesHost, ListDone, OpenDone, SearchDone};
pub use model::{AgentState, FolderTarget, PaneView, ProjectView, TabView, WorkspaceView};
pub use settings::{ConfigUpdate, Settings, SettingsHost};
pub use update::{UpdateHost, UpdateOffer, Updates};

use crate::bridge::Link;
use crate::shell::Shell;
use crate::theme::{Theme, ThemeKind, sync_component_theme};

/// The catalog's command ids and titles, which the settings file's `[keys]`
/// table names (RF-CFG-03).
///
/// ```
/// assert!(yien_ui::commands().iter().any(|(id, _)| *id == "settings.open_config"));
/// ```
pub fn commands() -> Vec<(&'static str, &'static str)> {
    catalog::CATALOG.iter().map(|c| (c.id, c.title)).collect()
}

/// Opens the main window and runs until it closes. `client` carries messages
/// to the core and `events` brings its events back; closing the window quits,
/// which drops `client` and lets the core end its panes.
///
/// `attention` shows toasts and the taskbar badge when an agent needs the
/// user while the window is in the background (RF-ATN-09, -10).
///
/// `settings` brings the settings file's readings, applied live
/// (RF-CFG-01), and opens the file.
///
/// `files` lists, searches and opens the selected tab's files for the
/// context bar (RF-CTX-01, RF-CTX-03).
///
/// `updates` brings each newer YIEN the update check finds, and downloads
/// and installs one when the user asks.
///
/// `on_quit` runs once when the app quits, including at Windows logoff or
/// shutdown, where gpui ends the process without returning from `run`.
///
/// ```no_run
/// # fn connect() -> (Box<dyn yien_ui::CoreClient>, tokio::sync::mpsc::Receiver<yien_proto::CoreMsg>) { unimplemented!() }
/// # fn os() -> std::rc::Rc<dyn yien_ui::OsAttention> { unimplemented!() }
/// # fn settings() -> yien_ui::Settings { unimplemented!() }
/// # fn files() -> yien_ui::Files { unimplemented!() }
/// # fn updates() -> yien_ui::Updates { unimplemented!() }
/// let (client, events) = connect();
/// yien_ui::run(client, events, os(), settings(), files(), updates(), || println!("bye"));
/// ```
pub fn run(
    client: Box<dyn CoreClient>,
    events: mpsc::Receiver<CoreMsg>,
    attention: Rc<dyn OsAttention>,
    settings: Settings,
    files: Files,
    updates: Updates,
    on_quit: impl FnOnce() + 'static,
) {
    gpui_kit::application()
        .with_assets(assets::YienAssets)
        .run(move |cx| {
            let mut on_quit = Some(on_quit);
            cx.on_app_quit(move |_| {
                if let Some(on_quit) = on_quit.take() {
                    on_quit();
                }
                async {}
            })
            .detach();
            gpui_kit::init(cx);
            // gpui-kit's own keys, bound again whenever the settings rebind ours.
            let base_keys: Vec<KeyBinding> =
                cx.key_bindings().borrow().bindings().cloned().collect();
            actions::bind_keys(cx);
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            // The window opens in the default palette; the settings are
            // applied before its first frame.
            sync_component_theme(&Theme::of(ThemeKind::default()), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1440.0), px(900.0)), cx)),
                window_min_size: Some(size(px(960.0), px(600.0))),
                ..TitleBar::window_options()
            };
            let link = Rc::new(Link::new(client));
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| {
                    Shell::new(
                        link, events, attention, settings, files, updates, base_keys, window, cx,
                    )
                })
            });
            match opened {
                Ok(_) => cx.activate(true),
                Err(error) => {
                    tracing::error!("could not open the main window: {error:#}");
                    cx.quit();
                }
            }
        });
}
