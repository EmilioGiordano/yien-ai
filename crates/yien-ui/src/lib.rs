//! The YIEN window, drawn with GPUI: projects and their vertical tabs on the
//! left, the selected tab's panes in the centre, the context bar on the right.
//!
//! The UI never talks to the core directly (ADR-03). It draws a
//! [`WorkspaceView`], plain data keyed by `yien-proto` ids.

#![forbid(unsafe_code)]

mod actions;
mod assets;
mod fonts;
mod model;
mod shell;
mod theme;

use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext, WindowBounds, WindowOptions, px, size};

pub use model::{AgentState, PaneView, ProjectView, TabView, ViewError, WorkspaceView};

use crate::fonts::Fonts;
use crate::shell::Shell;
use crate::theme::{Theme, ThemeKind, sync_component_theme};

/// Opens the main window showing `workspace` and runs until it closes.
///
/// `on_quit` runs once when the app quits, including at Windows logoff or
/// shutdown, where gpui ends the process without returning from `run`.
///
/// ```no_run
/// # fn demo() -> yien_ui::WorkspaceView { unimplemented!() }
/// yien_ui::run(demo(), || println!("bye"));
/// ```
pub fn run(workspace: WorkspaceView, on_quit: impl FnOnce() + 'static) {
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
            let theme = Theme::of(ThemeKind::default());
            sync_component_theme(&theme, cx);
            let fonts = Fonts::resolve(&cx.text_system().all_font_names());
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1440.0), px(900.0)), cx)),
                window_min_size: Some(size(px(960.0), px(600.0))),
                ..TitleBar::window_options()
            };
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| Shell::new(workspace, theme, fonts, window, cx))
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
