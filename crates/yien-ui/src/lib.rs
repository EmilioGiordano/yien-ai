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
mod bridge;
mod fonts;
mod grid;
mod layout;
mod model;
mod shell;
mod terminal;
mod theme;

use std::rc::Rc;

use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext, WindowBounds, WindowOptions, px, size};
use tokio::sync::mpsc;
use yien_proto::CoreMsg;

pub use bridge::{CoreClient, Refused};
pub use model::{AgentState, PaneView, ProjectView, TabView, WorkspaceView};

use crate::bridge::Link;
use crate::fonts::Fonts;
use crate::shell::Shell;
use crate::theme::{Theme, ThemeKind, sync_component_theme};

/// Opens the main window and runs until it closes. `client` carries messages
/// to the core and `events` brings its events back; closing the window quits,
/// which drops `client` and lets the core end its panes.
///
/// `on_quit` runs once when the app quits, including at Windows logoff or
/// shutdown, where gpui ends the process without returning from `run`.
///
/// ```no_run
/// # fn connect() -> (Box<dyn yien_ui::CoreClient>, tokio::sync::mpsc::Receiver<yien_proto::CoreMsg>) { unimplemented!() }
/// let (client, events) = connect();
/// yien_ui::run(client, events, || println!("bye"));
/// ```
pub fn run(
    client: Box<dyn CoreClient>,
    events: mpsc::Receiver<CoreMsg>,
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
            actions::bind_keys(cx);
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            let theme = Theme::of(ThemeKind::default());
            sync_component_theme(&theme, cx);
            let fonts = Fonts::resolve(&cx.text_system().all_font_names());
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1440.0), px(900.0)), cx)),
                window_min_size: Some(size(px(960.0), px(600.0))),
                ..TitleBar::window_options()
            };
            let link = Rc::new(Link::new(client));
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| Shell::new(link, events, theme, fonts, window, cx))
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
