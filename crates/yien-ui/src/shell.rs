//! The main window: title bar, tab bar, pane area, context bar and status bar,
//! laid out as in the prototype, kept in step with the core's events.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::f32::consts::FRAC_PI_2;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::TitleBar;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tokio::sync::mpsc;
use yien_proto::{
    ClientMsg, Command, CoreMsg, Direction, Orientation, PaneId, ProfileId, ProjectId,
    SHELL_PROFILE, SideBars, TabGit, TabId, TerminalPalette,
};

use crate::actions::*;
use crate::assets::{self, state_icon};
use crate::attention::{Alerts, OsAttention, launch_pane};
use crate::bridge::{Link, core_busy, deliver};
use crate::catalog::{CATALOG, Scope, shortcut};
use crate::file_tree::join_relative;
use crate::files::Files;
use crate::fonts::Fonts;
use crate::git::{ContextGit, FileMark};
use crate::keys::Rebinds;
use crate::layout::neighbor;
use crate::model::{
    AgentState, FolderTarget, PaneView, ProjectView, TabView, WorkspaceView, folder_label,
    frames_to_take, typed_tab_name,
};
use crate::palette::{
    Entry, Ink, KeyboardTo, Mode, Palette, Row, Target, command_entries, ink_runs,
    keyboard_after_close, keyboard_pane, session_entries, switcher_entries, switcher_home,
};
use crate::pane_head::{self, HeadText, PaneHeadLayout, program_tag, runs_agent};
use crate::region::{self, DividerKey, Modality, Region};
use crate::settings::{Settings, SettingsHost, SettingsModel, profiles_notice};
use crate::sidebars::{self, Bar, Drawn};
use crate::terminal::{TerminalEvent, TerminalPane};
use crate::theme::{Theme, ThemeKind};
use crate::title_bar::{
    PILL_LEAST, PILL_SHARE, PILL_WIDTH, ROW_RIGHT_PADDING, TitleLayout, centring_margins,
};
use crate::tray::{CloseRequest, close_request, quit_detail};
use crate::update::Updates;

mod bars;
mod default_tab;
mod dividers;
mod files;
mod menu;
mod pane_menu;
mod settings;
mod status;
mod tab_controls;
mod tab_drag;
mod tray;
mod update;
mod worktrees;
mod zoom;

use dividers::{DividerGrab, PaneArea};
use files::{ContextView, FilesPanel};
use pane_menu::{MenuKind, PaneMenu, PaneMenuKey};
use status::{Line, Notice};
use tab_drag::{RowMark, TabDragState};
use tray::Tray;

/// The palette's width and its gap below the window top, as the prototype's.
const PALETTE_WIDTH: f32 = 620.0;
const PALETTE_TOP: f32 = 56.0;
const PALETTE_LIST_HEIGHT: f32 = 380.0;
/// The left padding of a pane row under its tab in the switcher's tree.
const PALETTE_INDENT: f32 = 20.0;
const CONFIRM_WIDTH: f32 = 440.0;
/// Test-only: when set, the window opens with the attention tray open, so a
/// window check can capture it without sending keys.
const TRAY_SEED_VAR: &str = "YIEN_TEST_ATTENTION_TRAY";
const TITLE_BAR_HEIGHT: f32 = 40.0;
const STATUS_BAR_HEIGHT: f32 = 26.0;
/// Half the 6 px gutter between panes: every cell is inset by it.
const HALF_GUTTER: f32 = 3.0;
/// How much one key press grows or shrinks a pane, as a fraction of its split.
const RESIZE_STEP: f32 = 0.05;
/// Events applied in one go; the rest wait for the next turn of the loop.
const MAX_BATCH: usize = 1024;
/// The profile the Claude shortcuts start.
const CLAUDE_PROFILE: &str = "claude";
/// The longest folder a pane head shows before cutting its first names.
const PANE_FOLDER_CHARS: usize = 48;
const SIGNAL_LOST: &str = "signal lost";
const SHELL_NAME_SHRINK: f32 = 1000.0;

pub(crate) struct Shell {
    workspace: WorkspaceView,
    theme: Theme,
    /// The terminal palette the core has taken, `None` until it took one.
    palette_sent: Option<TerminalPalette>,
    fonts: Fonts,
    /// The terminals' font size from the settings, zoomed (RF-CFG-02).
    terminal_size: f32,
    /// Pixels the user zoomed the terminals by over the settings' size.
    terminal_zoom: i32,
    /// The zoom the core holds for the session; `None` until its snapshot
    /// brings the saved one.
    terminal_zoom_saved: Option<i32>,
    /// The last valid settings and the session's theme toggle.
    settings: SettingsModel,
    settings_host: Box<dyn SettingsHost>,
    /// The notice the last settings reading put up, so the next valid one
    /// can take it down.
    settings_notice: Option<String>,
    /// The notice for profile files skipped at start (RF-AGE-04).
    profiles_notice: Option<String>,
    /// The installed font families, listed again at each settings reading.
    installed_fonts: Vec<String>,
    /// gpui-kit's bindings, and the settings' rebinds now in the keymap
    /// on top of them (RF-CFG-03).
    base_keys: Vec<KeyBinding>,
    rebinds: Rebinds,
    /// The side bars as the user left them (RF-NAV-10).
    side_bars: SideBars,
    /// The side bars the core holds for the session; `None` until its
    /// snapshot brings the saved ones.
    side_bars_saved: Option<SideBars>,
    /// How wide the bars were drawn at the last render.
    bar_widths: Drawn,
    /// How far right of a bar's edge the pointer grabbed it, so a drag
    /// moves the edge without jumping to the pointer.
    bar_grab: f32,
    /// What the context bar shows under the branch head.
    context_view: ContextView,
    /// The Files view (RF-CTX-01).
    files: FilesPanel,
    focus: FocusHandle,
    title: String,
    link: Rc<Link>,
    terminals: HashMap<PaneId, Entity<TerminalPane>>,
    /// The window's subscription to each terminal's right clicks.
    terminal_events: HashMap<PaneId, Subscription>,
    /// The pane the keyboard was last moved to.
    keyboard_on: Option<PaneId>,
    /// What the status bar shows in place of the key hints, if anything.
    notice: Option<Notice>,
    /// A standing notice a passing one covers, shown again when it goes.
    behind: Option<Notice>,
    /// Counts the notices shown, so a stale timer cannot clear a newer one.
    notice_generation: u64,
    /// The pane whose split is choosing a profile, and which way (RF-PAN-18).
    split_menu: Option<(PaneId, Orientation)>,
    /// The chooser a mouse-down outside it just closed, and when. The click
    /// that follows on its own button must not open it again.
    menu_closed: Option<((PaneId, Orientation), std::time::Instant)>,
    /// The pane or terminal menu, while open (RF-PAN-20).
    menu: Option<PaneMenu>,
    /// The menu a mouse-down outside it just closed, and when.
    menu_closed_at: Option<(PaneMenuKey, Instant)>,
    /// Where each pane head was painted, for the menu opened by keyboard.
    pane_heads: Rc<RefCell<HashMap<PaneId, Bounds<Pixels>>>>,
    /// The user's home folder, which pane heads show as `~`.
    home: Option<String>,
    /// The command palette or the switcher, while open.
    palette: Option<OpenPalette>,
    /// The last past-sessions request sent; each list waits for its own.
    session_requests: u64,
    /// The question asked before closing a project that has work running,
    /// or quitting while agents work (RF-SES-13).
    confirm: Option<Confirm>,
    /// The user chose to quit with agents working; closing no longer asks.
    quit_confirmed: bool,
    /// The attention tray, while open (RF-ATN-13).
    tray: Option<Tray>,
    /// When a mouse-down outside the tray last closed it. The click that
    /// follows on the counter must not open it again.
    tray_closed_at: Option<Instant>,
    /// The tab whose name is being edited in place (RF-ESP-16).
    renaming: Option<Renaming>,
    /// The prompt for a new worktree tab's branch (RF-ESP-05).
    worktree_prompt: Option<worktrees::Prompt>,
    /// The question before closing the last tab on a worktree (RF-ESP-07).
    worktree_close: Option<worktrees::Closing>,
    /// Toasts, the taskbar badge and the flash (RF-ATN-09, -10).
    alerts: Alerts,
    /// A newer YIEN: its notice, notes and install.
    updates: update::UpdatePanel,
    /// The tab bar and the context bar as places the keyboard can be
    /// (RF-NAV-08); the pane area is the terminals' own focus.
    tab_bar_focus: FocusHandle,
    context_focus: FocusHandle,
    /// The tab row the arrows are on while the tab bar has the keyboard.
    tab_cursor: Option<TabId>,
    /// A tab row being dragged to another place in its project (RF-ESP-16).
    tab_drag: TabDragState,
    /// The region that held the keyboard at the last render.
    keyboard_region: Region,
    /// Whether the last move came from the keyboard or a mouse button; the
    /// bar rings show only for the keyboard.
    modality: Modality,
    /// F6 put the keyboard in a bar and it has not been in the pane area
    /// since. Only then does the bar keep it when the core moves the pane
    /// focus; a click in a bar focuses it too, and the pane must still get
    /// the keyboard after it.
    keyboard_in_bar: bool,
    /// The divider the pointer took (RF-PAN-02).
    divider_grab: Option<DividerGrab>,
    /// The dividers as a place the keyboard can be, and the one it is on.
    divider_focus: FocusHandle,
    divider_cursor: Option<DividerKey>,
    pane_area: PaneArea,
    _subscriptions: Vec<Subscription>,
}

/// A tab name being edited in its row.
struct Renaming {
    tab: TabId,
    input: Entity<InputState>,
    _input_events: Subscription,
}

/// A question the window asks before acting, over everything else.
struct Confirm {
    asked: Asked,
    focus: FocusHandle,
}

/// What the window asks about.
enum Asked {
    /// Closing a project that has work running.
    CloseProject { project: ProjectId, name: String },
    /// Closing a tab whose panes run work (RF-ESP-03).
    CloseTab { tab: TabId, name: String },
    /// Closing a pane that runs work.
    ClosePane { pane: PaneId, name: String },
    /// Quitting while agents work (RF-SES-13).
    Quit,
}

/// The palette's model and the widgets drawing it.
struct OpenPalette {
    model: Palette,
    input: Entity<InputState>,
    scroll: ScrollHandle,
    _input_events: Subscription,
}

impl Shell {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        link: Rc<Link>,
        mut events: mpsc::Receiver<CoreMsg>,
        os: Rc<dyn OsAttention>,
        settings: Settings,
        files: Files,
        updates: Updates,
        base_keys: Vec<KeyBinding>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        // Bindings are scoped to the root's key context, which only takes part in
        // dispatch while something inside the root holds focus.
        window.focus(&focus, cx);
        cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = events.recv().await {
                let mut batch = vec![first];
                while batch.len() < MAX_BATCH {
                    match events.try_recv() {
                        Ok(msg) => batch.push(msg),
                        Err(_) => break,
                    }
                }
                let applied =
                    this.update_in(cx, |shell, window, cx| shell.apply(batch, window, cx));
                if applied.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |shell, cx| {
                shell.fail("The YIEN core stopped. Restart YIEN.", cx);
            });
        })
        .detach();
        let focused = window.is_window_active();
        deliver(&link, ClientMsg::WindowFocus { focused }, cx);
        let activation = cx.observe_window_activation(window, |shell, window, cx| {
            let focused = window.is_window_active();
            shell.send(ClientMsg::WindowFocus { focused }, cx);
            if !focused {
                shell.cancel_tab_drag(window, cx);
            }
            shell.alerts.refresh_badge();
            shell.update_alerts(window, cx);
        });
        if let Ok(handle) = HasWindowHandle::window_handle(window)
            && let RawWindowHandle::Win32(win32) = handle.as_raw()
        {
            os.attach(win32.hwnd.get());
        }
        let (clicked, mut clicks) = mpsc::unbounded_channel::<String>();
        os.on_activate(Box::new(move |launch| {
            let _ = clicked.send(launch);
        }));
        cx.spawn_in(window, async move |this, cx| {
            while let Some(launch) = clicks.recv().await {
                let handled = this.update_in(cx, |shell, window, cx| {
                    shell.toast_clicked(&launch, window, cx)
                });
                if handled.is_err() {
                    return;
                }
            }
        })
        .detach();
        Self::listen_to_settings(settings.updates, window, cx);
        let updates = Self::update_panel(updates, window, cx);
        let appearance = cx.observe_window_appearance(window, |shell, window, cx| {
            shell.apply_look(window, cx);
        });
        // Before any binding: Escape must end a tab drag without reaching
        // the pane, where it would interrupt the agent.
        let shell = cx.weak_entity();
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.key == "escape" {
                let _ = shell.update(cx, |shell, cx| {
                    if shell.cancel_tab_drag(window, cx) {
                        cx.stop_propagation();
                    }
                });
            }
        });
        let files = FilesPanel::new(files, window, cx);
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |shell, cx| shell.window_should_close(window, cx))
                .unwrap_or(true)
        });
        let tray = std::env::var_os(TRAY_SEED_VAR).map(|_| Tray::new(cx.focus_handle()));
        let mut shell = Self {
            workspace: WorkspaceView::default(),
            // `apply_config` below sets both from the settings.
            theme: Theme::of(ThemeKind::default()),
            palette_sent: None,
            fonts: Fonts::default(),
            terminal_size: crate::terminal::DEFAULT_FONT_SIZE,
            terminal_zoom: 0,
            terminal_zoom_saved: None,
            settings: SettingsModel::default(),
            settings_host: settings.host,
            settings_notice: None,
            profiles_notice: None,
            installed_fonts: Vec::new(),
            base_keys,
            rebinds: Rebinds::new(),
            side_bars: SideBars::default(),
            side_bars_saved: None,
            bar_widths: sidebars::drawn(&SideBars::default(), 0.0),
            bar_grab: 0.0,
            context_view: if files.seeded() {
                ContextView::Files
            } else {
                ContextView::Changes
            },
            files,
            focus,
            title: String::new(),
            link,
            terminals: HashMap::new(),
            terminal_events: HashMap::new(),
            keyboard_on: None,
            notice: None,
            behind: None,
            notice_generation: 0,
            split_menu: None,
            menu_closed: None,
            menu: None,
            menu_closed_at: None,
            pane_heads: Rc::default(),
            home: home_folder(),
            palette: None,
            session_requests: 0,
            confirm: None,
            quit_confirmed: false,
            tray,
            tray_closed_at: None,
            renaming: None,
            worktree_prompt: None,
            worktree_close: None,
            alerts: Alerts::new(os),
            updates,
            tab_bar_focus: cx.focus_handle(),
            context_focus: cx.focus_handle(),
            tab_cursor: None,
            tab_drag: TabDragState::default(),
            keyboard_region: Region::Panes,
            modality: Modality::Keyboard,
            keyboard_in_bar: false,
            divider_grab: None,
            divider_focus: cx.focus_handle(),
            divider_cursor: None,
            pane_area: PaneArea::default(),
            _subscriptions: vec![activation, appearance, escape],
        };
        shell.profiles_notice = profiles_notice(&settings.profile_problems);
        if let Some(text) = shell.profiles_notice.clone() {
            shell.show(Notice::warning(text), cx);
        }
        shell.apply_config(settings.initial, window, cx);
        if let Some(tray) = &shell.tray {
            window.focus(tray.focus(), cx);
        }
        shell
    }

    fn send(&self, msg: ClientMsg, cx: &mut Context<Self>) {
        deliver(&self.link, msg, cx);
    }

    fn command(&self, command: Command, cx: &mut Context<Self>) {
        self.send(ClientMsg::Command(command), cx);
    }

    /// Applies a batch of core events, then takes the frame of every pane
    /// that announced one. Events for panes or tabs the UI no longer knows
    /// are dropped.
    fn apply(&mut self, batch: Vec<CoreMsg>, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_palette(cx);
        let mut ready: BTreeSet<PaneId> = BTreeSet::new();
        let mut changed = false;
        let mut git_changed = Vec::new();
        for msg in batch {
            match msg {
                CoreMsg::PaneFrameReady { pane } => {
                    ready.insert(pane);
                    continue;
                }
                CoreMsg::WorkspaceSnapshot(snapshot) => {
                    self.workspace = WorkspaceView::from_snapshot(&snapshot);
                    self.side_bars = snapshot.side_bars;
                    self.side_bars_saved = Some(snapshot.side_bars);
                    self.take_saved_zoom(snapshot.terminal_zoom, window, cx);
                    self.workspace
                        .set_group_by_state(self.settings.groups_by_state());
                }
                CoreMsg::ProjectUpdated(info) => self.workspace.project_updated(&info),
                CoreMsg::ProjectClosed { project } => {
                    for pane in self.workspace.project_closed(project) {
                        ready.remove(&pane);
                    }
                }
                CoreMsg::TabUpdated(info) => {
                    self.workspace.tab_updated(&info);
                    self.divider_settled(info.id);
                }
                CoreMsg::TabClosed { tab } => {
                    for pane in self.workspace.tab_closed(tab) {
                        ready.remove(&pane);
                    }
                }
                CoreMsg::ActiveTab { tab } => self.workspace.set_active(tab),
                CoreMsg::PaneUpdated(info) => self.workspace.pane_updated(&info),
                CoreMsg::PaneClosed { pane } => {
                    ready.remove(&pane);
                    self.workspace.pane_closed(pane);
                }
                CoreMsg::PaneState { pane, state } => {
                    self.workspace.pane_state(pane, state);
                }
                CoreMsg::PaneExited { pane, code } => {
                    self.workspace.pane_exited(pane, code);
                }
                CoreMsg::SelectionText { pane, text } => {
                    let text = text.filter(|t| !t.is_empty());
                    tracing::debug!(
                        pane = pane.get(),
                        copied = text.is_some(),
                        "selection answered"
                    );
                    if let Some(text) = text {
                        let text = crate::grid::clipboard_text(&text, cfg!(windows));
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    continue;
                }
                CoreMsg::TabGit { tab, git } => {
                    if !self.workspace.tab_git(tab, git) {
                        continue;
                    }
                    git_changed.push(tab);
                }
                CoreMsg::NotificationsMuted { muted } => {
                    self.workspace.set_notifications_muted(muted);
                }
                CoreMsg::Error { pane, message } => {
                    self.fail(message, cx);
                    if pane.is_none() {
                        self.workspace.command_refused();
                    }
                    // Only an error about no pane can answer a palette
                    // request; a pane's own error leaves the list waiting.
                    if pane.is_none()
                        && let Some(open) = self.palette.as_mut()
                    {
                        open.model.stop_waiting();
                    }
                }
                CoreMsg::ClipboardReady { pane } => {
                    let name = self.workspace.pane(pane).name;
                    let enabled = self.settings.config().osc52_copy;
                    if let Some((text, notice)) = crate::settings::osc52_copy(
                        || self.link.take_clipboard(pane),
                        enabled,
                        &name,
                        cfg!(windows),
                    ) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        let showing = self.notice.as_ref().map(|n| n.kind);
                        if status::copy_notice_replaces(showing) {
                            self.say(notice, cx);
                        }
                    }
                    continue;
                }
                CoreMsg::Notice { message } => self.say(message, cx),
                CoreMsg::WorktreeChecked { tab, check, .. } => self.worktree_checked(tab, check),
                CoreMsg::AgentSessions {
                    project,
                    request,
                    sessions,
                } => {
                    self.sessions_listed(project, request, &sessions, cx);
                    continue;
                }
            }
            changed = true;
        }
        if changed {
            self.drop_stale_questions(window, cx);
            self.sync_tray();
            self.sync_terminals(window, cx);
        }
        let take = frames_to_take(&ready, changed, &self.visible_panes(), |p| {
            self.terminals.contains_key(&p)
        });
        self.take_frames(take, cx);
        if changed {
            self.sync_focus(window, cx);
            self.update_alerts(window, cx);
            self.sync_files(window, cx);
            self.files_git_changed(&git_changed, cx);
            cx.notify();
        }
    }

    /// Updates the badge and, when a pane started needing the user while the
    /// window is in the background, schedules the toast for when the
    /// coalescing window closes.
    fn update_alerts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = window.is_window_active();
        let Some(at) = self.alerts.update(&self.workspace, focused, Instant::now()) else {
            return;
        };
        let wait = at.saturating_duration_since(Instant::now());
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update_in(cx, |shell, window, _| {
                let focused = window.is_window_active();
                let now = Instant::now().max(at);
                shell.alerts.flush(&shell.workspace, focused, now);
            });
        })
        .detach();
    }

    /// The user clicked YIEN's toast: bring the window forward on the pane it
    /// named, or on the pane waiting longest if that one moved on.
    fn toast_clicked(&mut self, launch: &str, window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!("toast clicked");
        window.activate_window();
        let waiting = self.workspace.waiting_panes();
        let pane = launch_pane(launch)
            .filter(|p| waiting.contains(p))
            .or_else(|| waiting.first().copied());
        if let Some(pane) = pane {
            self.command(Command::FocusPane { pane }, cx);
        }
    }

    /// The panes of the selected tab that are drawn now.
    fn visible_panes(&self) -> Vec<PaneId> {
        self.workspace
            .selected_tab()
            .map(|t| t.rects().into_iter().map(|(p, _)| p).collect())
            .unwrap_or_default()
    }

    /// Takes the latest frame of each pane from the core. The core sends no
    /// new notice for a pane until its frame is taken, so a pane with a view
    /// is always taken; `None` means nothing new.
    fn take_frames(&self, panes: impl IntoIterator<Item = PaneId>, cx: &mut Context<Self>) {
        for pane in panes {
            let Some(terminal) = self.terminals.get(&pane) else {
                continue;
            };
            if let Some(frame) = self.link.take_frame(pane) {
                terminal.update(cx, |t, cx| t.set_frame(frame, cx));
            }
        }
    }

    /// One terminal view per pane the workspace holds; views of closed panes go.
    fn sync_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.workspace.all_panes();
        self.terminals.retain(|id, _| panes.contains(id));
        self.terminal_events.retain(|id, _| panes.contains(id));
        self.pane_heads
            .borrow_mut()
            .retain(|id, _| panes.contains(id));
        for pane in panes {
            if self.terminals.contains_key(&pane) {
                continue;
            }
            let link = Rc::clone(&self.link);
            let family = self.fonts.terminal.clone();
            let size = self.terminal_size;
            let theme = self.theme;
            let view = cx.new(|cx| TerminalPane::new(pane, link, family, size, theme, window, cx));
            let events = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
                let TerminalEvent::ContextMenu { at, has_selection } = *event;
                this.open_menu(pane, MenuKind::Terminal, at, has_selection, window, cx);
            });
            self.terminal_events.insert(pane, events);
            self.terminals.insert(pane, view);
        }
    }

    /// A question about a project that is gone no longer applies.
    fn drop_stale_questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gone = self.confirm.as_ref().is_some_and(|c| match &c.asked {
            Asked::CloseProject { project, .. } => self.workspace.project(*project).is_none(),
            Asked::CloseTab { tab, .. } => self.workspace.tab(*tab).is_none(),
            Asked::ClosePane { pane, .. } => self.workspace.tab_of(*pane).is_none(),
            Asked::Quit => false,
        });
        if gone {
            self.confirm = None;
            self.give_back_keyboard(window, cx);
        }
        let renamed_gone = self
            .renaming
            .as_ref()
            .is_some_and(|r| self.workspace.tab(r.tab).is_none());
        if renamed_gone {
            self.renaming = None;
            self.give_back_keyboard(window, cx);
        }
        self.drop_stale_worktree_question(window, cx);
        self.drop_stale_menu(window, cx);
    }

    /// Whether an overlay holds the keyboard; closing it gives it back.
    fn overlay_has_keyboard(&self) -> bool {
        self.palette.is_some()
            || self.menu.is_some()
            || self.tray.is_some()
            || self.confirm.is_some()
            || self.renaming.is_some()
            || self.worktree_prompt.is_some()
            || self.worktree_close.is_some()
            || self.updates.notes_open()
    }

    /// Moves the keyboard to the selected tab's focused pane when that
    /// changes. A bar that holds the keyboard keeps it until the user leaves.
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keep_divider_cursor(window, cx);
        self.keyboard_in_bar =
            region::bar_claim_holds(self.keyboard_in_bar, self.region(window, cx));
        if self.overlay_has_keyboard() || self.keyboard_in_bar {
            return;
        }
        let target = keyboard_pane(&self.workspace);
        if target == self.keyboard_on {
            return;
        }
        self.keyboard_on = target;
        match target.and_then(|p| self.terminals.get(&p)) {
            Some(terminal) => {
                let handle = terminal.read(cx).handle().clone();
                window.focus(&handle, cx);
            }
            None => window.focus(&self.focus, cx),
        }
    }

    fn focused_pane(&self) -> Option<PaneId> {
        self.workspace.selected_tab().map(|t| t.focused)
    }

    /// The region that holds the keyboard; the pane area when no bar does.
    fn region(&self, window: &Window, cx: &App) -> Region {
        if self.tab_bar_focus.contains_focused(window, cx) {
            Region::TabBar
        } else if self.context_focus.contains_focused(window, cx) {
            Region::ContextBar
        } else if self.divider_focus.is_focused(window) {
            Region::Dividers
        } else {
            Region::Panes
        }
    }

    /// Any mouse button hides the keyboard rings until a key moves the
    /// keyboard again; the keyboard itself stays where the click put it.
    fn on_pointer_down(&mut self, cx: &mut Context<Self>) {
        if self.modality != Modality::Pointer {
            self.modality = Modality::Pointer;
            cx.notify();
        }
    }

    /// Calls `on_pointer_down` for every mouse button in the window. Not a
    /// capture listener on the root: gpui runs those only while the root is
    /// hovered, and an `occlude`d element on top (the icon buttons) hides it.
    fn watch_pointer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = cx.entity().downgrade();
        canvas(
            |_, _, _| (),
            move |_, (), window, _| {
                let down = shell.clone();
                window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        let _ = down.update(cx, |this, cx| this.on_pointer_down(cx));
                    }
                });
                // Wherever the button comes up, a tab drag ends there.
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        let clicks = event.click_count;
                        let _ = shell.update(cx, |this, cx| this.end_tab_drag(clicks, window, cx));
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    /// Whether `bar` draws its keyboard ring at this render (RF-NAV-08).
    pub(super) fn ringed(&self, bar: Region) -> bool {
        region::ring_on(bar, self.keyboard_region, self.modality)
    }

    /// F6 and Shift+F6 (RF-NAV-08). An overlay keeps the keyboard.
    fn move_region(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay_has_keyboard() {
            return;
        }
        self.modality = Modality::Keyboard;
        let from = self.region(window, cx);
        let to = region::next_region(
            from,
            forward,
            self.bar_shown(Bar::Tab),
            self.bar_shown(Bar::Context),
            !self.divider_keys(cx).is_empty(),
        );
        match to {
            Region::TabBar => {
                let rows = self.workspace.drawn_tabs();
                let selected = self.workspace.selected_tab().map(|t| t.id);
                self.tab_cursor = region::cursor_on_entry(&rows, self.tab_cursor, selected);
                window.focus(&self.tab_bar_focus, cx);
                self.keyboard_in_bar = true;
            }
            Region::ContextBar if self.context_view == ContextView::Files => {
                self.focus_files(window, cx);
            }
            Region::ContextBar => {
                window.focus(&self.context_focus, cx);
                self.keyboard_in_bar = true;
            }
            Region::Dividers => self.focus_dividers(forward, window, cx),
            Region::Panes => self.give_back_keyboard(window, cx),
        }
        cx.notify();
    }

    fn focus_next_region(
        &mut self,
        _: &FocusNextRegion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_region(true, window, cx);
    }

    fn focus_previous_region(
        &mut self,
        _: &FocusPreviousRegion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_region(false, window, cx);
    }

    fn move_tab_cursor(&mut self, by: isize, cx: &mut Context<Self>) {
        self.modality = Modality::Keyboard;
        let rows = self.workspace.drawn_tabs();
        let selected = self.workspace.selected_tab().map(|t| t.id);
        self.tab_cursor = region::move_cursor(&rows, self.tab_cursor, selected, by);
        cx.notify();
    }

    fn tab_bar_up(&mut self, _: &TabBarUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_tab_cursor(-1, cx);
    }

    fn tab_bar_down(&mut self, _: &TabBarDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_tab_cursor(1, cx);
    }

    /// Enter on a tab row: select it, as a click would, and give the
    /// keyboard to its pane.
    fn tab_bar_select(&mut self, _: &TabBarSelect, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.workspace.drawn_tabs();
        if let Some(tab) = self.tab_cursor.filter(|t| rows.contains(t)) {
            self.select_tab(tab, window, cx);
        }
        self.give_back_keyboard(window, cx);
    }

    /// Puts up a plain notice, which takes itself down after a few seconds.
    /// A warning or an error stays until `clear_notice` or a newer notice.
    pub(super) fn say(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.show(Notice::info(text), cx);
    }

    /// Puts up an error, which stays until it is dismissed or replaced.
    pub(super) fn fail(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.show(Notice::error(text), cx);
    }

    pub(super) fn show(&mut self, notice: Notice, cx: &mut Context<Self>) {
        self.notice_generation += 1;
        let shown = self.notice_generation;
        let lifetime = notice.lifetime();
        self.behind = status::behind(self.notice.take(), self.behind.take(), &notice);
        self.notice = Some(notice);
        if let Some(lifetime) = lifetime {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(lifetime).await;
                let _ = this.update(cx, |shell, cx| {
                    if status::timer_clears(shell.notice.as_ref(), shown, shell.notice_generation) {
                        shell.clear_notice(cx);
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn dismiss_notice(&mut self, _: &DismissNotice, _: &mut Window, cx: &mut Context<Self>) {
        if !self.clear_notice(cx) {
            cx.propagate();
        }
    }

    /// Takes the notice down; whether there was one. A standing notice it
    /// covered, or else the update notice, comes back.
    pub(super) fn clear_notice(&mut self, cx: &mut Context<Self>) -> bool {
        let taken = self.notice.take();
        self.notice_generation += 1;
        if let Some(taken) = &taken {
            self.update_notice_cleared(taken);
        }
        if let Some(next) = self.behind.take().or_else(|| self.update_standing_notice()) {
            self.show(next, cx);
        }
        cx.notify();
        taken.is_some()
    }

    fn tab_bar_leave(&mut self, _: &TabBarLeave, window: &mut Window, cx: &mut Context<Self>) {
        self.give_back_keyboard(window, cx);
    }

    fn context_bar_leave(
        &mut self,
        _: &ContextBarLeave,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.give_back_keyboard(window, cx);
    }

    /// The tab row to ring, while the tab bar has the keyboard.
    fn ringed_tab(&self) -> Option<TabId> {
        self.tab_cursor.filter(|_| self.ringed(Region::TabBar))
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.workspace.tab_after(1) {
            self.select_tab(tab, window, cx);
        }
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.workspace.tab_after(-1) {
            self.select_tab(tab, window, cx);
        }
    }

    /// The core shows a tab's panes only while it is the active one, so the
    /// selection goes through it; the local change just avoids a blank frame.
    fn select_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.select_tab(id) {
            self.command(Command::SelectTab { tab: id }, cx);
            let take = frames_to_take(&BTreeSet::new(), true, &self.visible_panes(), |p| {
                self.terminals.contains_key(&p)
            });
            self.take_frames(take, cx);
            self.sync_focus(window, cx);
            cx.notify();
        }
    }

    /// Folds or unfolds a project at once; the core saves it and confirms.
    fn toggle_fold(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        if let Some(collapsed) = self.workspace.toggle_fold(id) {
            self.command(
                Command::SetProjectCollapsed {
                    project: id,
                    collapsed,
                },
                cx,
            );
            cx.notify();
        }
    }

    fn split(&mut self, orientation: Orientation, before: bool, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_pane() {
            self.split_pane(pane, orientation, before, None, cx);
        }
    }

    /// Starts `profile` in the new pane, the shell when `None` (RF-PAN-18);
    /// `before` puts it left of or above `pane`.
    fn split_pane(
        &mut self,
        pane: PaneId,
        orientation: Orientation,
        before: bool,
        profile: Option<ProfileId>,
        cx: &mut Context<Self>,
    ) {
        self.split_menu = None;
        self.command(
            Command::SplitPane {
                pane,
                orientation,
                profile,
                before,
            },
            cx,
        );
        cx.notify();
    }

    fn split_right_claude(&mut self, _: &SplitRightClaude, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_pane() {
            let claude = Some(ProfileId::new(CLAUDE_PROFILE));
            self.split_pane(pane, Orientation::Horizontal, false, claude, cx);
        }
    }

    fn toggle_split_menu(
        &mut self,
        pane: PaneId,
        orientation: Orientation,
        cx: &mut Context<Self>,
    ) {
        let open = Some((pane, orientation));
        let just_closed = self.menu_closed.take().is_some_and(|(menu, at)| {
            Some(menu) == open && at.elapsed() < std::time::Duration::from_millis(500)
        });
        self.split_menu = if self.split_menu == open || just_closed {
            None
        } else {
            open
        };
        cx.notify();
    }

    /// Opens a tab in the selected tab's project, running `profile`.
    /// With every tab closed it falls back to the project selected last.
    fn new_tab_with(&mut self, profile: Option<&str>, cx: &mut Context<Self>) {
        match self.workspace.project_for_new_tab() {
            Some(project) => self.new_tab_in(project, profile, cx),
            None => {
                self.say("There is no project to open a tab in.".to_owned(), cx);
            }
        }
    }

    fn new_tab(&mut self, _: &NewTab, _: &mut Window, cx: &mut Context<Self>) {
        self.new_tab_with(None, cx);
    }

    fn new_claude_tab(&mut self, _: &NewClaudeTab, _: &mut Window, cx: &mut Context<Self>) {
        self.new_tab_with(Some(CLAUDE_PROFILE), cx);
    }

    /// Focuses the next pane that needs the user, oldest first (RF-ATN-05).
    /// The core selects its tab; the red stays until the agent moves on
    /// (RF-ATN-04).
    fn jump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tray(window, cx);
        match self.workspace.next_waiting(self.focused_pane()) {
            Some(pane) => self.command(Command::FocusPane { pane }, cx),
            None => {
                self.say("Nobody is waiting for you.".to_owned(), cx);
            }
        }
    }

    fn jump_to_needs_you(
        &mut self,
        _: &JumpToNeedsYou,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jump(window, cx);
    }

    fn split_right(&mut self, _: &SplitRight, _: &mut Window, cx: &mut Context<Self>) {
        self.split(Orientation::Horizontal, false, cx);
    }

    fn split_down(&mut self, _: &SplitDown, _: &mut Window, cx: &mut Context<Self>) {
        self.split(Orientation::Vertical, false, cx);
    }

    fn split_left(&mut self, _: &SplitLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.split(Orientation::Horizontal, true, cx);
    }

    fn split_up(&mut self, _: &SplitUp, _: &mut Window, cx: &mut Context<Self>) {
        self.split(Orientation::Vertical, true, cx);
    }

    /// Closes the focused pane; closing the last tab on a worktree asks
    /// first whether to keep the worktree (RF-ESP-07).
    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_pane() {
            self.close_pane_id(pane, window, cx);
        }
    }

    /// Closes `pane` as the `pane.close` key does: asking first while it
    /// runs work, then on the last tab on a worktree.
    fn close_pane_id(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.close_pane_step(pane, false, window, cx);
    }

    fn focus_direction(&mut self, direction: Direction, cx: &mut Context<Self>) {
        self.command(Command::FocusDirection { direction }, cx);
    }

    fn focus_left(&mut self, _: &FocusLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Direction::Left, cx);
    }

    fn focus_right(&mut self, _: &FocusRight, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Direction::Right, cx);
    }

    fn focus_up(&mut self, _: &FocusUp, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Direction::Up, cx);
    }

    fn focus_down(&mut self, _: &FocusDown, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Direction::Down, cx);
    }

    fn resize(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_pane() {
            self.command(Command::ResizeSplit { pane, delta }, cx);
        }
    }

    fn grow_pane(&mut self, _: &GrowPane, _: &mut Window, cx: &mut Context<Self>) {
        self.resize(RESIZE_STEP, cx);
    }

    fn shrink_pane(&mut self, _: &ShrinkPane, _: &mut Window, cx: &mut Context<Self>) {
        self.resize(-RESIZE_STEP, cx);
    }

    fn swap(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let Some(tab) = self.workspace.selected_tab() else {
            return;
        };
        let a = tab.focused;
        if let Some(b) = neighbor(&tab.rects(), a, direction) {
            self.command(Command::SwapPanes { a, b }, cx);
        }
    }

    fn swap_left(&mut self, _: &SwapLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.swap(Direction::Left, cx);
    }

    fn swap_right(&mut self, _: &SwapRight, _: &mut Window, cx: &mut Context<Self>) {
        self.swap(Direction::Right, cx);
    }

    fn swap_up(&mut self, _: &SwapUp, _: &mut Window, cx: &mut Context<Self>) {
        self.swap(Direction::Up, cx);
    }

    fn swap_down(&mut self, _: &SwapDown, _: &mut Window, cx: &mut Context<Self>) {
        self.swap(Direction::Down, cx);
    }

    fn toggle_maximize(&mut self, _: &ToggleMaximize, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_pane() {
            self.command(Command::ToggleMaximize { pane }, cx);
        }
    }

    /// Tab `n` of the selected tab's project (RF-NAV-05).
    fn go_to_tab(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.workspace.nth_tab_of_selected_project(n) {
            self.select_tab(tab, window, cx);
        }
    }

    fn toggle_command_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_palette(Mode::Commands, window, cx);
    }

    fn toggle_switcher(&mut self, _: &ToggleSwitcher, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_palette(Mode::Switcher, window, cx);
    }

    /// Lists the selected project's past agent sessions (RF-SES-07). The
    /// list opens at once and fills when the core has read them.
    fn resume_past_session(
        &mut self,
        _: &ResumePastSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_palette(Mode::Sessions, window, cx);
    }

    /// Fills the open sessions list with the core's answer for its project.
    fn sessions_listed(
        &mut self,
        project: ProjectId,
        request: u64,
        sessions: &[yien_proto::AgentSession],
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.palette.as_mut() else {
            return;
        };
        if open.model.sessions_of() != Some(project) {
            return;
        }
        let entries = session_entries(project, sessions, &self.workspace, now_ms());
        if open.model.set_sessions(project, request, entries) {
            open.scroll.scroll_to_item(0);
            cx.notify();
        }
    }

    /// Opens the palette in `mode`, or closes it when it is already open in
    /// that mode. Typing then goes to its input, not the terminal.
    fn toggle_palette(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .palette
            .as_ref()
            .is_some_and(|open| open.model.mode() == mode)
        {
            self.close_palette(window, cx);
            return;
        }
        let sessions_of = match mode {
            Mode::Sessions => match self.workspace.project_for_new_tab() {
                Some(project) => Some(project),
                None => {
                    self.say("Add a project to list its past sessions.".to_owned(), cx);
                    return;
                }
            },
            _ => None,
        };
        self.split_menu = None;
        self.confirm = None;
        self.finish_rename(true, window, cx);
        let commands = {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            command_entries(
                |at| {
                    let spec = CATALOG.get(at)?;
                    shortcut(&keymap, (spec.action)().as_ref())
                },
                self.workspace.profiles(),
            )
        };
        let commands: Vec<Entry> = commands
            .into_iter()
            .filter(|entry| self.update_lists(entry))
            .collect();
        let places = switcher_entries(&self.workspace);
        let placeholder = match mode {
            Mode::Commands => "Execute a command…",
            Mode::Switcher => "Go to a tab, pane or project. Type > for commands",
            Mode::Sessions => "Resume a past session…",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let input_events = cx.subscribe_in(&input, window, Self::on_palette_input);
        window.focus(&input.focus_handle(cx), cx);
        let model = match sessions_of {
            Some(project) => {
                self.session_requests += 1;
                let request = self.session_requests;
                self.command(Command::ListAgentSessions { project, request }, cx);
                Palette::sessions(project, request)
            }
            None => Palette::new(mode, commands, places).with_home(switcher_home(&self.workspace)),
        };
        self.palette = Some(OpenPalette {
            model,
            input,
            scroll: ScrollHandle::new(),
            _input_events: input_events,
        });
        // A new list's first prepaint drops a scroll request (gpui learns the
        // overflow after it), and next-frame callbacks run before that
        // frame's draw: ask two frames on, once the list has been drawn.
        cx.on_next_frame(window, |_, window, cx| {
            cx.on_next_frame(window, |this, _, cx| {
                if let Some(open) = &this.palette {
                    open.scroll
                        .scroll_to_item(open.model.selected().unwrap_or(0));
                    cx.notify();
                }
            });
        });
        cx.notify();
    }

    fn on_palette_input(
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
        if let Some(open) = self.palette.as_mut() {
            open.model.set_query(&query);
            open.scroll
                .scroll_to_item(open.model.selected().unwrap_or(0));
            cx.notify();
        }
    }

    /// Closes the palette and gives the keyboard to the pane the core says
    /// is focused, which may have moved while the palette was open.
    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_none() {
            return;
        }
        self.give_back_keyboard(window, cx);
    }

    /// Gives the keyboard to the pane the core says is focused, or to the
    /// window when no tab is open, after an overlay held it.
    fn give_back_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keyboard_in_bar = false;
        let terminal = match keyboard_after_close(&self.workspace) {
            KeyboardTo::Pane(pane) => self.terminals.get(&pane).map(|t| (pane, t)),
            KeyboardTo::Window => None,
        };
        match terminal {
            Some((pane, terminal)) => {
                self.keyboard_on = Some(pane);
                let handle = terminal.read(cx).handle().clone();
                window.focus(&handle, cx);
            }
            None => {
                // No view yet: the next sync moves the keyboard once it exists.
                self.keyboard_on = None;
                window.focus(&self.focus, cx);
            }
        }
        cx.notify();
    }

    /// Runs row `at`, or the selected row when `None`. The palette closes
    /// first, so a command acts on the pane that has the keyboard again.
    fn run_palette(&mut self, at: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = &self.palette else {
            return;
        };
        let target = match at {
            Some(at) => open.model.target_at(at),
            None => open.model.target(),
        };
        let Some(target) = target else {
            return;
        };
        self.close_palette(window, cx);
        match target {
            Target::Command(at) => {
                if let Some(spec) = CATALOG.get(at) {
                    window.dispatch_action((spec.action)(), cx);
                }
            }
            Target::Tab(tab) => self.select_tab(tab, window, cx),
            Target::Project(project) => {
                let tab = self
                    .workspace
                    .tabs_by_recent()
                    .iter()
                    .find(|t| t.project == project)
                    .map(|t| t.id);
                if let Some(tab) = tab {
                    self.select_tab(tab, window, cx);
                }
            }
            Target::Pane(pane) => self.command(Command::FocusPane { pane }, cx),
            Target::Split(profile, orientation) => {
                if let Some(pane) = self.focused_pane() {
                    self.split_pane(pane, orientation, false, Some(profile), cx);
                }
            }
            Target::NewTab(profile) => self.new_tab_with(Some(profile.as_str()), cx),
            Target::Resume {
                project,
                profile,
                session_id,
            } => self.command(
                Command::ResumeAgentSession {
                    project,
                    profile,
                    session_id,
                },
                cx,
            ),
        }
    }

    /// Relaunches the focused pane's program once it has stopped
    /// (RF-PAN-19); a running program is left alone.
    fn relaunch_pane(&mut self, _: &RelaunchPane, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.focused_pane() else {
            return;
        };
        let pane = self.workspace.pane(id);
        if pane.exited {
            self.command(Command::RelaunchPane { pane: id }, cx);
        } else {
            self.say(format!("{} has not exited.", pane.name), cx);
        }
    }

    /// Asks for a folder with the system's picker, which runs off the UI
    /// thread, and registers it as a project (RF-ESP-01). Cancel does nothing.
    fn add_project(&mut self, _: &AddProject, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match chosen.await {
                Ok(Ok(Some(paths))) => Ok(paths.into_iter().next()),
                Ok(Ok(None)) | Err(_) => Ok(None),
                Ok(Err(error)) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.folder_picked(picked, window, cx);
            });
        })
        .detach();
    }

    /// A folder that is a project already selects that project; any other
    /// becomes a new project with a shell tab, selected by the core.
    fn folder_picked(
        &mut self,
        picked: Result<Option<PathBuf>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = match picked {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(error) => {
                self.fail(format!("The folder picker failed: {error}"), cx);
                return;
            }
        };
        let Some(root) = path.to_str() else {
            self.fail(
                format!(
                    "{} cannot be a project: its name is not valid Unicode.",
                    path.display()
                ),
                cx,
            );
            return;
        };
        match self.workspace.folder_target(root) {
            FolderTarget::Tab(tab) => self.select_tab(tab, window, cx),
            FolderTarget::NewProject => self.command(
                Command::NewProject {
                    root: root.to_owned(),
                    name: None,
                },
                cx,
            ),
        }
    }

    /// Closes the selected tab's project, or the one selected last.
    fn close_project(&mut self, _: &CloseProject, window: &mut Window, cx: &mut Context<Self>) {
        match self.workspace.project_for_new_tab() {
            Some(project) => self.request_close(project, window, cx),
            None => {
                self.say("There is no project to close.".to_owned(), cx);
            }
        }
    }

    /// Closes a project at once when only idle shells would stop; otherwise
    /// asks first, saying how many panes would stop.
    fn request_close(&mut self, project: ProjectId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.workspace.project(project).map(|p| p.name.clone()) else {
            return;
        };
        let busy = self.workspace.busy_panes_of(project);
        if busy == 0 {
            self.command(Command::CloseProject { project }, cx);
            return;
        }
        self.ask(Asked::CloseProject { project, name }, window, cx);
    }

    /// Puts a question over the window and gives it the keyboard.
    fn ask(&mut self, asked: Asked, window: &mut Window, cx: &mut Context<Self>) {
        self.split_menu = None;
        self.tray = None;
        self.palette = None;
        // Both draw over the window and would hide the question.
        self.worktree_prompt = None;
        self.worktree_close = None;
        self.finish_rename(true, window, cx);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.confirm = Some(Confirm { asked, focus });
        cx.notify();
    }

    /// The OS asks to close the window (the title bar's close button,
    /// Alt+F4). While agents work the window stays and asks first
    /// (RF-SES-13); logoff and shutdown never come through here.
    fn window_should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let asking = self
            .confirm
            .as_ref()
            .is_some_and(|c| matches!(c.asked, Asked::Quit));
        match close_request(self.workspace.working_panes(), asking, self.quit_confirmed) {
            CloseRequest::Close => {
                self.quit_confirmed = true;
                true
            }
            CloseRequest::Ask(working) => {
                tracing::info!(working, "quit asked while agents work");
                self.ask(Asked::Quit, window, cx);
                false
            }
            CloseRequest::StillAsking => false,
        }
    }

    /// `app: quit YIEN`: closes the window, which quits, asking first while
    /// agents work (RF-SES-13). Pressed again, or held, while the question
    /// is open it keeps asking.
    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.window_should_close(window, cx) {
            window.remove_window();
        }
    }

    /// While the question is open, a window command closes it and goes no
    /// further, so a key cannot act on a workspace hidden behind it.
    fn close_on_window_commands(
        &self,
        dialog: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        CATALOG
            .iter()
            .filter(|spec| spec.scope == Scope::Window && spec.id != "app.quit")
            .fold(dialog, |dialog, spec| {
                dialog.on_boxed_action(
                    &*(spec.action)(),
                    cx.listener(|this, _: &dyn Action, window, cx| {
                        this.confirm_cancel(&ConfirmCancel, window, cx);
                    }),
                )
            })
    }

    /// Enter on the question: yes, quitting included.
    fn confirm_accept(&mut self, _: &ConfirmAccept, window: &mut Window, cx: &mut Context<Self>) {
        self.accept_confirm(window, cx);
    }

    /// The question's yes button.
    fn accept_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        match confirm.asked {
            Asked::CloseProject { project, .. } => {
                self.command(Command::CloseProject { project }, cx);
                self.give_back_keyboard(window, cx);
            }
            Asked::CloseTab { tab, .. } => {
                self.give_back_keyboard(window, cx);
                self.close_tab_step(tab, true, window, cx);
            }
            Asked::ClosePane { pane, .. } => {
                self.give_back_keyboard(window, cx);
                self.close_pane_step(pane, true, window, cx);
            }
            Asked::Quit => {
                tracing::info!("quit anyway while agents work");
                self.quit_confirmed = true;
                window.remove_window();
            }
        }
    }

    fn confirm_cancel(&mut self, _: &ConfirmCancel, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(confirm) = self.confirm.take() {
            if matches!(confirm.asked, Asked::Quit) {
                self.quit_cancelled(cx);
            }
            self.give_back_keyboard(window, cx);
        }
    }

    /// The question before closing a project that has work running, over
    /// the window like the palette. Enter closes it, Escape keeps it.
    fn render_confirm(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let confirm = self.confirm.as_ref()?;
        let t = self.theme;
        let hover = t.hover;
        let (title, detail, accept, id) = match &confirm.asked {
            Asked::CloseProject { project, name } => {
                let detail = match self.workspace.busy_panes_of(*project) {
                    0 => {
                        "Nothing in it is running any more. Nothing on disk is deleted.".to_owned()
                    }
                    1 => "1 pane is still running an agent or a program; closing the project \
                          stops it. Nothing on disk is deleted."
                        .to_owned(),
                    n => format!(
                        "{n} panes are still running an agent or a program; closing the \
                         project stops them. Nothing on disk is deleted."
                    ),
                };
                (
                    format!("Close {name}?"),
                    detail,
                    "Close project",
                    "confirm-close-project",
                )
            }
            Asked::CloseTab { tab, name } => (
                format!("Close {name}?"),
                tab_controls::close_tab_detail(self.workspace.busy_panes_of_tab(*tab)),
                "Close tab",
                "confirm-close-tab",
            ),
            Asked::ClosePane { pane, name } => (
                format!("Close {name}?"),
                tab_controls::close_pane_detail(self.workspace.pane(*pane).is_busy()).to_owned(),
                "Close pane",
                "confirm-close-pane",
            ),
            Asked::Quit => (
                "Quit YIEN?".to_owned(),
                quit_detail(&self.workspace),
                "Quit anyway",
                "confirm-quit",
            ),
        };
        let button = |id: &'static str, label: &'static str, keys: &'static str| {
            h_flex()
                .id(id)
                .role(Role::Button)
                .aria_label(format!("{label} ({keys})"))
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(t.line)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(label)
                .child(key_hint(keys, &t))
        };
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
                    v_flex()
                        .id(id)
                        .role(Role::Dialog)
                        .aria_label(title.clone())
                        .track_focus(&confirm.focus)
                        .key_context(CONFIRM_CONTEXT)
                        .mt(px(PALETTE_TOP))
                        .w(px(CONFIRM_WIDTH))
                        .max_w(relative(0.8))
                        .gap(px(10.0))
                        .p(px(16.0))
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(t.line)
                        .bg(t.raised)
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.confirm_cancel(&ConfirmCancel, window, cx);
                        }))
                        .map(|dialog| self.close_on_window_commands(dialog, cx))
                        .child(
                            div()
                                .text_size(t.text(15.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(t.text(13.0))
                                .text_color(t.muted)
                                .child(detail),
                        )
                        .child(
                            h_flex()
                                .justify_end()
                                .gap(px(8.0))
                                .pt(px(4.0))
                                .text_size(t.text(13.0))
                                .child(button("confirm-cancel", "Cancel", "Esc").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.confirm_cancel(&ConfirmCancel, window, cx);
                                    }),
                                ))
                                .child(
                                    button("confirm-accept", accept, "Enter")
                                        .font_weight(FontWeight::MEDIUM)
                                        .border_color(t.muted)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.accept_confirm(window, cx);
                                        })),
                                ),
                        ),
                ),
        )
    }

    fn rename_tab(&mut self, _: &RenameTab, window: &mut Window, cx: &mut Context<Self>) {
        self.modality = Modality::Keyboard;
        match self.workspace.selected_tab().map(|t| t.id) {
            Some(tab) => self.start_rename(tab, window, cx),
            None => {
                self.say("There is no tab to rename.".to_owned(), cx);
            }
        }
    }

    /// Turns the tab's name in its row into a field holding the name, all
    /// selected. The tab bar opens and the project unfolds to show it.
    fn start_rename(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((name, project)) = self.workspace.tab(tab).map(|t| (t.name.clone(), t.project))
        else {
            return;
        };
        if self.confirm.is_some() {
            return;
        }
        self.finish_rename(true, window, cx);
        self.split_menu = None;
        self.change_side_bars(|b| sidebars::show(b, Bar::Tab), window, cx);
        if self.workspace.is_folded(project) {
            self.toggle_fold(project, cx);
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |state, cx| state.select_all(window, cx));
        let input_events = cx.subscribe_in(&input, window, Self::on_rename_input);
        window.focus(&input.focus_handle(cx), cx);
        self.renaming = Some(Renaming {
            tab,
            input,
            _input_events: input_events,
        });
        cx.notify();
    }

    /// Clicking away keeps the name typed, as Enter does.
    fn on_rename_input(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Blur) {
            self.finish_rename(true, window, cx);
        }
    }

    /// Ends the edit; `keep` sends the typed name, and an empty one brings
    /// back the automatic name.
    fn finish_rename(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        let current = self.workspace.tab(renaming.tab).map(|t| t.name.as_str());
        let typed = renaming.input.read(cx).value();
        if let Some(name) = keep.then(|| typed_tab_name(&typed, current)).flatten() {
            self.command(
                Command::RenameTab {
                    tab: renaming.tab,
                    name,
                },
                cx,
            );
        }
        self.give_back_keyboard(window, cx);
    }

    fn rename_confirm(&mut self, _: &RenameConfirm, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_rename(true, window, cx);
    }

    fn rename_cancel(&mut self, _: &RenameCancel, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_rename(false, window, cx);
    }

    fn toggle_project_fold(
        &mut self,
        _: &ToggleProjectFold,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.workspace.selected_project().map(|p| p.id) {
            self.toggle_fold(project, cx);
        }
    }

    /// Toasts and flashes only; the red mark, the counter and the badge stay
    /// (RF-ATN-16). The core saves the choice and confirms it.
    fn toggle_notifications_muted(
        &mut self,
        _: &ToggleNotificationsMuted,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let muted = !self.workspace.notifications_muted();
        self.workspace.set_notifications_muted(muted);
        self.say(mute_notice(muted, "Notifications"), cx);
        self.command(Command::SetNotificationsMuted { muted }, cx);
        cx.notify();
    }

    fn toggle_tab_muted(&mut self, _: &ToggleTabMuted, _: &mut Window, cx: &mut Context<Self>) {
        let Some((tab, muted)) = self.workspace.selected_tab().map(|t| (t.id, !t.muted)) else {
            self.say("There is no tab to mute.".to_owned(), cx);
            return;
        };
        self.say(mute_notice(muted, "This tab's notifications"), cx);
        self.command(Command::SetTabMuted { tab, muted }, cx);
        cx.notify();
    }

    fn palette_up(&mut self, _: &PaletteUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_palette(-1, cx);
    }

    fn palette_down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_palette(1, cx);
    }

    fn move_palette(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(open) = self.palette.as_mut() {
            open.model.move_by(delta);
            if let Some(at) = open.model.selected() {
                open.scroll.scroll_to_item(at);
            }
            cx.notify();
        }
    }

    fn palette_confirm(&mut self, _: &PaletteConfirm, window: &mut Window, cx: &mut Context<Self>) {
        self.run_palette(None, window, cx);
    }

    fn palette_close(&mut self, _: &PaletteClose, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
    }

    /// The window title carries the counter so the taskbar shows it, like the
    /// prototype's document title.
    fn sync_title(&mut self, window: &mut Window) {
        let waiting = self.workspace.needs_you_panes();
        let title = if waiting > 0 {
            format!("({waiting}) YIEN")
        } else {
            "YIEN".to_owned()
        };
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
    }

    fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let layout = TitleLayout::at(f32::from(window.viewport_size().width), t.text(1.0).into());
        let crumbs = self.workspace.selected_tab().map(|tab| {
            let project = self
                .workspace
                .selected_project()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            h_flex()
                .gap(px(4.0))
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(t.text(13.0))
                .text_color(t.muted)
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(project),
                )
                .child("/")
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(t.text)
                        .font_weight(FontWeight::MEDIUM)
                        .child(tab.name.clone()),
                )
        });
        TitleBar::new()
            // Linux only: there the close button removes the window without
            // `WM_CLOSE`'s question (RF-SES-13).
            .on_close_window(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.quit(&Quit, window, cx);
            }))
            .h(px(TITLE_BAR_HEIGHT))
            .bg(t.panel)
            .border_color(t.line)
            .child(
                h_flex()
                    .flex_1()
                    .w_0()
                    .min_w_0()
                    .overflow_x_hidden()
                    .h_full()
                    .items_center()
                    .gap(px(12.0))
                    .pr(px(ROW_RIGHT_PADDING))
                    .child(
                        h_flex()
                            .flex_1()
                            .when(layout.centred, |side| side.mr(px(centring_margins().0)))
                            .items_center()
                            .gap(px(12.0))
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(px(8.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child(img(assets::MARK).size(px(20.0)))
                                    .child("YIEN"),
                            )
                            .child(div().flex_1().w_0().min_w_0().children(crumbs)),
                    )
                    .child(self.render_palette_pill(&layout, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .items_center()
                            .justify_end()
                            .gap(px(12.0))
                            .when(layout.centred, |side| side.ml(px(centring_margins().1)))
                            .child(self.render_need_count(cx))
                            .when(layout.theme_button, |bar| {
                                bar.child(
                                    icon_button("toggle-theme", assets::THEME, &t)
                                        .aria_label(format!(
                                            "Toggle sumi and washi theme ({THEME_KEYS})"
                                        ))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_theme(&ToggleTheme, window, cx)
                                        })),
                                )
                            })
                            .when(layout.tab_bar_button, |bar| {
                                bar.child(
                                    icon_button("toggle-tab-bar", assets::PANEL_LEFT, &t)
                                        .aria_label(format!("Toggle tab bar ({TAB_BAR_KEYS})"))
                                        .aria_toggled(toggled(self.bar_shown(Bar::Tab)))
                                        .when(!self.bar_shown(Bar::Tab), |b| b.opacity(0.55))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_tab_bar(&ToggleTabBar, window, cx)
                                        })),
                                )
                            })
                            .when(layout.context_bar_button, |bar| {
                                bar.child(
                                    icon_button("toggle-context-bar", assets::PANEL_RIGHT, &t)
                                        .aria_label(format!(
                                            "Toggle context bar ({CONTEXT_BAR_KEYS})"
                                        ))
                                        .aria_toggled(toggled(self.bar_shown(Bar::Context)))
                                        .when(!self.bar_shown(Bar::Context), |b| b.opacity(0.55))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_context_bar(&ToggleContextBar, window, cx)
                                        })),
                                )
                            }),
                    ),
            )
    }

    /// The title bar button that opens the palette, as the prototype's.
    fn render_palette_pill(
        &self,
        layout: &TitleLayout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        h_flex()
            .id("palette-pill")
            .occlude()
            .role(Role::Button)
            .aria_label(format!("Execute a command ({PALETTE_KEYS})"))
            .flex_none()
            .w(px(PILL_WIDTH))
            .max_w(relative(PILL_SHARE))
            .min_w(px(PILL_LEAST))
            .items_center()
            .gap(px(10.0))
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(t.line)
            .bg(t.bg)
            .text_size(t.text(13.0))
            .text_color(t.faint)
            .whitespace_nowrap()
            .overflow_hidden()
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_palette(Mode::Commands, window, cx);
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child("Execute a command"),
            )
            .when(layout.pill_keys, |pill| {
                pill.child(
                    div()
                        .flex_none()
                        .text_size(t.text(11.0))
                        .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                        .child(PALETTE_KEYS),
                )
            })
    }

    /// The command palette or the switcher over the window (RF-NAV-01,
    /// RF-NAV-04), laid out as the prototype's `.palette`.
    fn render_palette(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let open = self.palette.as_ref()?;
        let t = self.theme;
        let model = &open.model;
        let selected = model.selected();
        let tree = model.shows_tree();
        let rows: Vec<AnyElement> = model
            .rows()
            .iter()
            .enumerate()
            .filter_map(|(at, row)| {
                let entry = model.entry(row)?;
                Some(self.render_palette_row(at, row, entry, selected == Some(at), tree, cx))
            })
            .collect();
        let label = match model.listing() {
            Mode::Commands => "Command palette",
            Mode::Switcher => "Go to a tab, pane or project",
            Mode::Sessions => "Past sessions",
        };
        let project = model
            .sessions_of()
            .and_then(|id| self.workspace.project(id))
            .map_or("this project", |p| p.name.as_str());
        let none = rows.is_empty().then(|| {
            div()
                .p(px(14.0))
                .text_size(t.text(13.0))
                .text_color(t.muted)
                .child(model.empty_text(project))
        });
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
                    v_flex()
                        .id("palette")
                        .role(Role::Dialog)
                        .aria_label(label)
                        .key_context(PALETTE_CONTEXT)
                        .mt(px(PALETTE_TOP))
                        .w(px(PALETTE_WIDTH))
                        .max_w(relative(0.8))
                        .overflow_hidden()
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(t.line)
                        .bg(t.raised)
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.close_palette(window, cx);
                        }))
                        .child(
                            div()
                                .px(px(14.0))
                                .py(px(8.0))
                                .border_b_1()
                                .border_color(t.line)
                                .text_size(t.text(14.5))
                                .child(
                                    Input::new(&open.input)
                                        .appearance(false)
                                        .aria_label("Search"),
                                ),
                        )
                        .child(
                            v_flex()
                                .id("palette-list")
                                .role(Role::ListBox)
                                .track_scroll(&open.scroll)
                                .overflow_y_scroll()
                                .max_h(px(PALETTE_LIST_HEIGHT))
                                .p(px(6.0))
                                .children(rows)
                                .children(none),
                        ),
                ),
        )
    }

    fn render_palette_row(
        &self,
        at: usize,
        row: &Row,
        entry: &Entry,
        selected: bool,
        tree: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let (shown, dim) = entry.drawn(tree);
        let nested = shown.len() < entry.text.len();
        // Hits are offsets into the whole text, so a shortened text drops them.
        let hits: &[usize] = if nested { &[] } else { &row.hits };
        let style = |color: Rgba, weight: FontWeight| HighlightStyle {
            color: Some(color.into()),
            font_weight: Some(weight),
            ..HighlightStyle::default()
        };
        let runs = ink_runs(shown, dim, hits).into_iter().map(|(range, ink)| {
            let look = match ink {
                Ink::Text => style(t.text, FontWeight::NORMAL),
                Ink::Dim => style(t.muted, FontWeight::NORMAL),
                Ink::Hit => style(t.ai, FontWeight::BOLD),
            };
            (range, look)
        });
        let text = StyledText::new(shown.to_owned()).with_highlights(runs);
        let hint_color = match entry.state {
            Some(AgentState::NeedsYou) => t.shu,
            Some(AgentState::Error) => t.ochre,
            _ => t.faint,
        };
        let label = if entry.hint.is_empty() {
            entry.text.clone()
        } else {
            format!("{}, {}", entry.text, entry.hint)
        };
        h_flex()
            .id(("palette-row", at))
            .role(Role::ListBoxOption)
            .aria_label(label)
            .aria_selected(selected)
            .items_center()
            .gap(px(10.0))
            .px(px(10.0))
            .py(px(7.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .when(selected, |r| r.bg(t.hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.run_palette(Some(at), window, cx);
            }))
            .when(nested, |r| {
                r.pl(px(PALETTE_INDENT))
                    .child(div().flex_none().w(px(1.0)).h(px(16.0)).bg(t.muted))
            })
            .when_some(entry.state, |r, state| r.child(glyph(state, &t)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(t.text(13.5))
                    .child(text),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(t.text(12.0))
                    .text_color(hint_color)
                    .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                    .child(entry.hint.clone()),
            )
            .into_any_element()
    }

    /// The global "needs you" counter (RF-ATN-03). Red only while someone
    /// waits; a click opens the attention tray (RF-ATN-13), as in the
    /// prototype.
    fn render_need_count(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let waiting = self.workspace.needs_you_panes();
        let text = match waiting {
            0 => "Nobody is waiting".to_owned(),
            1 => "1 needs you".to_owned(),
            n => format!("{n} need you"),
        };
        h_flex()
            .id("need-count")
            .role(Role::Button)
            .aria_label(format!("{text}. Show the attention tray ({TRAY_KEYS})"))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| {
                this.counter_clicked(window, cx);
            }))
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(3.0))
            .rounded_full()
            .border_1()
            .text_size(t.text(12.5))
            .font_weight(FontWeight::MEDIUM)
            .map(|pill| {
                if waiting > 0 {
                    pill.border_color(t.shu)
                        .bg(t.shu_wash)
                        .text_color(t.shu)
                        .child(glyph(AgentState::NeedsYou, &t))
                } else {
                    pill.border_color(t.line).text_color(t.faint)
                }
            })
            .child(text)
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let selected = self.workspace.selected_tab().map(|tab| tab.id);
        let by_state = self.workspace.groups_by_state();
        let groups: Vec<AnyElement> = if by_state {
            self.render_state_sections(selected, cx)
        } else {
            self.render_project_groups(selected, cx)
        };
        let head = h_flex()
            .flex_none()
            .items_center()
            .pl(px(12.0))
            .pr(px(6.0))
            .pt(px(6.0))
            .text_size(t.text(11.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.faint)
            .child(div().flex_1().child(if by_state {
                "Tabs by state"
            } else {
                "Projects"
            }))
            .child(self.default_tab_button(cx))
            .child(
                icon_button("add-project", assets::PLUS, &t)
                    .aria_label(format!("Add project ({ADD_PROJECT_KEYS})"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.add_project(&AddProject, window, cx);
                    })),
            );
        v_flex()
            .id("tab-bar")
            .key_context(TAB_BAR_CONTEXT)
            .track_focus(&self.tab_bar_focus)
            .role(Role::Navigation)
            .aria_label("Projects and tabs")
            .relative()
            .w(px(self.bar_widths.tab_bar.unwrap_or(0.0)))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(head)
            .child(
                v_flex()
                    .id("tab-list")
                    .track_scroll(&self.tab_drag.scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(6.0))
                    .pb(px(10.0))
                    .children(groups),
            )
            .when(self.ringed(Region::TabBar), |bar| bar.child(focus_ring(&t)))
            .children(self.render_bar_edge(Bar::Tab, cx))
    }

    /// The tab bar grouped by state (RF-ATN-07): a head per state with its
    /// count, then its tabs, each named with its project.
    fn render_state_sections(
        &self,
        selected: Option<TabId>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = self.theme;
        self.workspace
            .state_sections()
            .into_iter()
            .map(|(state, tabs)| {
                let rows: Vec<AnyElement> = tabs
                    .iter()
                    .filter_map(|id| self.workspace.tab(*id))
                    .map(|tab| self.render_tab_row(tab, Some(tab.id) == selected, true, cx))
                    .collect();
                v_flex()
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(6.0))
                            .pt(px(10.0))
                            .pb(px(4.0))
                            .text_size(t.text(12.5))
                            .text_color(t.muted)
                            .child(glyph(state, &t))
                            .child(
                                div()
                                    .text_size(t.text(13.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.text)
                                    .child(state.label()),
                            )
                            .child(div().text_color(t.faint).child(tabs.len().to_string())),
                    )
                    .child(
                        v_flex()
                            .id(("tabs-in-state", state as usize))
                            .role(Role::TabList)
                            .aria_label(format!("{} tabs, {}", state.label(), tabs.len()))
                            .children(rows),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The tab bar by project (RF-ESP-02): each project's head, then its
    /// tabs unless it is folded.
    fn render_project_groups(
        &self,
        selected: Option<TabId>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.workspace
            .projects()
            .iter()
            .map(|project| {
                let folded = self.workspace.is_folded(project.id);
                v_flex()
                    .child(self.render_project_head(project, folded, cx))
                    .when(!folded, |group| {
                        group.child(
                            v_flex()
                                .id(("tabs-of", project.id.get()))
                                .role(Role::TabList)
                                .aria_label(format!("Tabs of {}", project.name))
                                .children(self.workspace.tabs_of(project).map(|tab| {
                                    self.render_tab_row(tab, Some(tab.id) == selected, false, cx)
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }

    fn render_project_head(
        &self,
        project: &ProjectView,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let waiting = self.workspace.needs_you_tabs_of(project);
        let id = project.id;
        let hover = t.hover;
        let label = if waiting > 0 {
            format!("{}, {waiting} need you", project.name)
        } else {
            project.name.clone()
        };
        let chevron = svg()
            .path(assets::CHEVRON)
            .size(px(11.0))
            .flex_none()
            .text_color(t.faint)
            .when(folded, |c| {
                c.with_transformation(Transformation::rotate(radians(-FRAC_PI_2)))
            });
        h_flex()
            .id(("project", id.get()))
            .role(Role::Button)
            .aria_label(label)
            .aria_expanded(!folded)
            .items_center()
            .gap(px(6.0))
            .px(px(6.0))
            .pt(px(10.0))
            .pb(px(4.0))
            .text_size(t.text(12.5))
            .text_color(t.muted)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(id, cx)))
            .child(chevron)
            .when(waiting > 0, |head| {
                head.child(glyph(AgentState::NeedsYou, &t))
            })
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(t.text(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.text)
                    .child(project.name.clone()),
            )
            .when(waiting > 0, |head| {
                head.child(
                    div()
                        .flex_none()
                        .text_size(t.text(12.0))
                        .text_color(t.shu)
                        .child(format!("{waiting} need you")),
                )
            })
            .child(self.new_tab_button(project, cx).ml_auto())
            .child(
                div()
                    .id(("close-project", id.get()))
                    .role(Role::Button)
                    .aria_label(format!("Close project {}", project.name))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(20.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.request_close(id, window, cx);
                    }))
                    .child(svg().path(assets::CLOSE).size(px(12.0)).text_color(t.faint)),
            )
    }

    /// A tab's row; `with_project` names its project before it, as the
    /// tab bar grouped by state does (RF-ATN-07).
    fn render_tab_row(
        &self,
        tab: &TabView,
        active: bool,
        with_project: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.theme;
        let state = self.workspace.tab_state(tab);
        let id = tab.id;
        let hover = t.hover;
        let second_line = active || matches!(state, AgentState::NeedsYou | AgentState::Error);
        let detail = self.workspace.tab_detail(tab);
        let project = with_project
            .then(|| self.workspace.project(tab.project).map(|p| p.name.clone()))
            .flatten();
        let name = match &project {
            Some(project) => format!("{project} / {}", tab.name),
            None => tab.name.clone(),
        };
        // Grouped by state, rows of several projects mix, so none is dragged.
        let draggable = !with_project && !self.renaming.as_ref().is_some_and(|r| r.tab == id);
        let drop_target = !with_project;
        let mark = self.row_mark(id);
        let close_shown = active || self.ringed_tab() == Some(id);
        div()
            .id(("tab", id.get()))
            .group(tab_controls::row_group(id))
            .role(Role::Tab)
            .aria_label(match tab.worktree {
                Some(_) => format!("{name}, worktree, {}", state.label()),
                None => format!("{name}, {}", state.label()),
            })
            .aria_selected(active)
            .relative()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .w_full()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .border_1()
            .cursor_pointer()
            .map(|row| {
                if active {
                    row.bg(t.raised).border_color(t.line)
                } else {
                    row.border_color(gpui_kit::transparent_black())
                        .hover(move |s| s.bg(hover))
                }
            })
            .when(self.ringed_tab() == Some(id), |row| {
                row.border_color(t.focus_ring())
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if this.renaming.as_ref().is_some_and(|r| r.tab == id) {
                    return;
                }
                if event.click_count() >= 2 {
                    this.start_rename(id, window, cx);
                } else {
                    this.select_tab(id, window, cx);
                }
            }))
            .when(draggable, |row| self.draggable_row(row, tab, cx))
            .when(drop_target, |row| self.drop_target_row(row, tab, cx))
            .when(mark == Some(RowMark::Dragged), |row| row.opacity(0.45))
            .when_some(mark.filter(|m| *m != RowMark::Dragged), |row, mark| {
                row.child(drop_line(mark == RowMark::LineAbove, &t))
            })
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(glyph(state, &t))
                    .when_some(project, |row, project| {
                        row.child(
                            div()
                                .flex_none()
                                .max_w(px(90.0))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_color(t.faint)
                                .child(format!("{project} /")),
                        )
                    })
                    .child(self.render_tab_name(tab))
                    .when(tab.worktree.is_some(), |row| {
                        row.child(
                            svg()
                                .flex_none()
                                .path(assets::BRANCH)
                                .size(px(12.0))
                                .text_color(t.ai),
                        )
                    })
                    .child(self.close_tab_button(id, &name, close_shown, cx)),
            )
            .when(second_line, |row| {
                row.child(
                    h_flex()
                        .pl(px(22.0))
                        .gap(px(8.0))
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_size(t.text(12.0))
                        .text_color(t.faint)
                        .child(
                            div()
                                .flex_none()
                                .text_color(t.label_color(state))
                                .child(state.label()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .when_some(self.fonts.mono.clone(), |d, family| {
                                    d.font_family(family)
                                })
                                .child(detail),
                        ),
                )
            })
            .when_some(t.tab_row_bar(state), |row, red| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(6.0))
                        .bottom(px(6.0))
                        .w(px(2.0))
                        .rounded(px(2.0))
                        .bg(red),
                )
            })
            .into_any_element()
    }

    /// The tab's name, or the field editing it.
    fn render_tab_name(&self, tab: &TabView) -> AnyElement {
        let t = self.theme;
        match self.renaming.as_ref().filter(|r| r.tab == tab.id) {
            Some(renaming) => div()
                .flex_1()
                .min_w_0()
                .key_context(RENAME_CONTEXT)
                .px(px(4.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(t.ai)
                .bg(t.bg)
                .child(
                    Input::new(&renaming.input)
                        .appearance(false)
                        .aria_label(format!("New name for {}", tab.name)),
                )
                .into_any_element(),
            None => div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .font_weight(FontWeight::MEDIUM)
                .child(tab.name.clone())
                .into_any_element(),
        }
    }

    fn render_panes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let area = div()
            .id("panes")
            .role(Role::Main)
            .aria_label("Panes")
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(t.bg)
            .p(px(HALF_GUTTER));
        if self.workspace.has_no_projects() {
            return self.render_no_project(area, cx);
        }
        let Some(tab) = self.workspace.selected_tab() else {
            return area
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.muted)
                .when_some(self.fonts.display.clone(), |d, family| {
                    d.font_family(family)
                })
                .text_size(t.text(18.0))
                .child("No tab is open");
        };
        let rects = match self.shown_layout(tab) {
            Some(shown) => tab.rects_in(&shown),
            None => tab.rects(),
        };
        let area_width = self.pane_area.get().0;
        let cells: Vec<AnyElement> = rects
            .into_iter()
            .map(|(id, rect)| {
                let pane = self.workspace.pane(id);
                // Before the first paint the area is unknown: draw it wide.
                let cell = if area_width > 0.0 {
                    area_width * rect.w - 2.0 * HALF_GUTTER
                } else {
                    f32::INFINITY
                };
                div()
                    .absolute()
                    .left(relative(rect.x))
                    .top(relative(rect.y))
                    .w(relative(rect.w))
                    .h(relative(rect.h))
                    .p(px(HALF_GUTTER))
                    .child(self.render_pane(tab, &pane, cell, cx))
                    .into_any_element()
            })
            .collect();
        let dividers = self.render_dividers(tab, cx);
        area.child(
            div()
                .relative()
                .size_full()
                .child(self.watch_pane_area())
                .children(cells)
                .children(dividers),
        )
    }

    /// The pane area with no project at all: an invitation to add one.
    fn render_no_project(&self, area: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = self.theme;
        let hover = t.hover;
        area.flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.0))
            .child(
                div()
                    .text_color(t.muted)
                    .when_some(self.fonts.display.clone(), |d, family| {
                        d.font_family(family)
                    })
                    .text_size(t.text(18.0))
                    .child("No project is open"),
            )
            .child(
                div()
                    .text_size(t.text(13.0))
                    .text_color(t.faint)
                    .child("Add a folder to start working in it."),
            )
            .child(
                h_flex()
                    .id("empty-add-project")
                    .role(Role::Button)
                    .aria_label(format!("Add project ({ADD_PROJECT_KEYS})"))
                    .items_center()
                    .gap(px(10.0))
                    .px(px(14.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.raised)
                    .text_size(t.text(13.5))
                    .text_color(t.text)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.add_project(&AddProject, window, cx);
                    }))
                    .child(svg().path(assets::PLUS).size(px(14.0)).text_color(t.muted))
                    .child("Add project")
                    .child(
                        div()
                            .text_size(t.text(11.5))
                            .text_color(t.faint)
                            .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                            .child(ADD_PROJECT_KEYS),
                    ),
            )
    }

    fn render_pane(
        &self,
        tab: &TabView,
        pane: &PaneView,
        cell: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let focused = pane.id == tab.focused;
        let needs = pane.state.needs_you();
        let (border, border_color) = t.pane_border(focused, needs);
        let state_color = match pane.state {
            AgentState::NeedsYou => t.shu,
            AgentState::Error => t.ochre,
            _ => t.term_muted,
        };
        let id = pane.id;
        let folder = pane.cwd.as_deref().unwrap_or(&tab.dir);
        let folder_shown = folder_label(
            folder,
            self.home.as_deref(),
            cfg!(windows),
            PANE_FOLDER_CHARS,
        );
        let tag = program_tag(pane);
        let text = HeadText {
            name: &pane.name,
            profile: &pane.profile,
            tag,
            agent: runs_agent(pane),
            folder_lead: &folder_shown.lead,
            folder_last: &folder_shown.last,
            state: pane.state.label(),
            signal_lost: pane.signal_lost.then_some(SIGNAL_LOST),
        };
        let shows = PaneHeadLayout::at(cell - 2.0 * border, f32::from(t.text(1.0)), &text);
        let separate = text.separate_name();
        // Text shrinks and ellipsizes; buttons never do, so a wrong width
        // guess cuts words, not a button.
        let words = |s: String| div().min_w_0().overflow_hidden().text_ellipsis().child(s);
        let primary = |s: String| {
            words(s)
                .text_color(t.term_text)
                .font_weight(FontWeight::MEDIUM)
        };
        let who = if separate && shows.profile {
            words(pane.profile.clone())
        } else if separate && shows.name {
            words(tag.to_owned())
        } else if separate {
            primary(tag.to_owned())
        } else {
            primary(pane.name.clone())
        }
        .when(!text.agent, |mut d| {
            // A shell's name counts only a few letters in the layout, so it
            // gives way before any other text.
            d.style().flex_shrink = Some(SHELL_NAME_SHRINK);
            d
        });
        let heads = Rc::clone(&self.pane_heads);
        let head = h_flex()
            .relative()
            .flex_none()
            .items_center()
            .gap(px(pane_head::GAP))
            .min_w_0()
            .overflow_hidden()
            .px(px(pane_head::PADDING))
            .py(px(5.0))
            .border_b_1()
            .border_color(t.term_line)
            .whitespace_nowrap()
            .text_size(t.text(pane_head::TEXT_SIZE))
            .text_color(t.term_muted)
            .when(focused, |h| h.bg(t.term_raised))
            .child(glyph(pane.state, &t))
            .when(separate && shows.name, |h| {
                h.child(primary(pane.name.clone()))
            })
            .child(who)
            .when(!separate && shows.profile, |h| {
                h.child(words(pane.profile.clone()))
            })
            .when(shows.folder_last, |h| {
                h.child(
                    // Only the lead shrinks, so a narrow head still shows
                    // the last folder.
                    h_flex()
                        .id(("pane-folder", id.get()))
                        .aria_label(format!("Folder {folder}"))
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(t.text(pane_head::FOLDER_SIZE))
                        .text_color(t.term_faint)
                        .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                        .when(shows.folder_lead, |f| {
                            f.child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(folder_shown.lead.clone()),
                            )
                        })
                        .child(div().flex_none().child(folder_shown.last.clone())),
                )
            })
            .child(div().flex_1())
            .when(shows.state_label, |h| {
                h.child(words(pane.state.label().to_owned()).text_color(state_color))
            })
            .when(pane.signal_lost && shows.signal_lost, |h| {
                h.child(
                    words(SIGNAL_LOST.to_owned())
                        .id(("signal-lost", pane.id.get()))
                        .role(Role::Status)
                        .aria_label("Signal lost: the agent's state cannot be told")
                        .text_color(t.ochre),
                )
            })
            .when(shows.split_buttons, |h| {
                h.child(self.split_button(pane, Orientation::Horizontal, cx))
                    .child(self.split_button(pane, Orientation::Vertical, cx))
            })
            .when(shows.menu_button, |h| {
                h.child(self.pane_menu_button(pane, cx))
            })
            .when(shows.close_button, |h| {
                h.child(self.pane_close_button(pane, cx))
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // Or the root, which tracks focus, takes the keyboard
                    // back from the menu as this mouse-down bubbles up.
                    window.prevent_default();
                    this.open_menu(id, MenuKind::Pane, event.position, false, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    this.focus_pane_from_head(id, window, cx);
                }),
            )
            .child(
                canvas(
                    move |bounds, _, _| {
                        heads.borrow_mut().insert(id, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        let menu = self
            .split_menu
            .filter(|(p, _)| *p == pane.id)
            .map(|(_, orientation)| self.render_split_menu(pane.id, orientation, cx));
        let exited = pane.exited.then(|| {
            let id = pane.id;
            let what = match pane.exit_code {
                Some(code) => format!("{} exited with code {code}.", pane.name),
                None => format!("{} is not running.", pane.name),
            };
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(4.0))
                .border_b_1()
                .border_color(t.term_line)
                .bg(t.term_raised)
                .text_size(t.text(12.0))
                .child(div().flex_1().min_w_0().text_color(t.ochre).child(what))
                .child(
                    div()
                        .id(("relaunch", id.get()))
                        .role(Role::Button)
                        .aria_label(format!("Relaunch {}", pane.name))
                        .flex_none()
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(t.term_strong)
                        .text_color(t.term_text)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.command(Command::RelaunchPane { pane: id }, cx);
                        }))
                        .child("Relaunch"),
                )
        });
        let body = match self.terminals.get(&pane.id) {
            Some(terminal) => terminal.clone().into_any_element(),
            None => div().flex_1().into_any_element(),
        };
        let mut pane_label = format!("{} pane", pane.name);
        if separate {
            pane_label.push_str(&format!(", {tag}"));
        }
        pane_label.push_str(&format!(", {}", pane.state.label()));
        if pane.signal_lost && !shows.signal_lost {
            pane_label.push_str(", signal lost");
        }
        if !shows.folder_last {
            pane_label.push_str(&format!(", in {folder}"));
        }
        v_flex()
            .id(("pane", pane.id.get()))
            .role(Role::Group)
            .aria_label(pane_label)
            .relative()
            .size_full()
            .overflow_hidden()
            .rounded(px(8.0))
            .bg(t.term_bg)
            .text_color(t.term_text)
            .border(px(border))
            .border_color(border_color)
            .child(head)
            .children(exited)
            .child(body)
            .children(menu)
    }

    /// A pane head button that opens the profile chooser for a split
    /// (RF-PAN-18), as in the prototype.
    fn split_button(
        &self,
        pane: &PaneView,
        orientation: Orientation,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = self.theme;
        let (key, icon, label, keys) = match orientation {
            Orientation::Horizontal => (
                "split-right",
                assets::SPLIT_RIGHT,
                "Split right",
                SPLIT_RIGHT_KEYS,
            ),
            Orientation::Vertical => (
                "split-down",
                assets::SPLIT_DOWN,
                "Split down",
                SPLIT_DOWN_KEYS,
            ),
        };
        let id = pane.id;
        let open = self.split_menu == Some((id, orientation));
        let hover = t.term_raised;
        div()
            .id((key, id.get()))
            .occlude()
            .role(Role::Button)
            .aria_label(format!(
                "{label} {}, choose a profile ({keys} for the shell)",
                pane.name
            ))
            .aria_expanded(open)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(20.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .when(open, |b| b.bg(t.term_raised))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_split_menu(id, orientation, cx);
            }))
            .child(svg().path(icon).size(px(14.0)).text_color(t.term_muted))
    }

    /// A click on a pane head gives that pane the focus and the keyboard,
    /// not the window root.
    fn focus_pane_from_head(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminals.get(&pane) {
            let handle = terminal.read(cx).handle().clone();
            window.focus(&handle, cx);
            self.keyboard_on = Some(pane);
            self.keyboard_in_bar = false;
        }
        self.command(Command::FocusPane { pane }, cx);
    }

    /// A 20 px pane head button, as the split buttons.
    fn head_button(&self, key: &'static str, pane: PaneId, icon: &'static str) -> Stateful<Div> {
        let t = self.theme;
        let hover = t.term_raised;
        div()
            .id((key, pane.get()))
            .occlude()
            .role(Role::Button)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(20.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .child(svg().path(icon).size(px(14.0)).text_color(t.term_muted))
    }

    /// Opens the pane's menu of actions (RF-PAN-20), as in the prototype.
    fn pane_menu_button(&self, pane: &PaneView, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = pane.id;
        let open = self.menu_open_for(id);
        let keys = {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            shortcut(&keymap, &OpenPaneMenu)
        };
        let label = match keys {
            Some(keys) => format!("Pane actions for {} ({keys})", pane.name),
            None => format!("Pane actions for {}", pane.name),
        };
        self.head_button("pane-actions", id, assets::DOTS)
            .aria_label(label)
            .aria_expanded(open)
            .when(open, |b| b.bg(self.theme.term_raised))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.pane_menu_clicked(id, event.position(), window, cx);
            }))
    }

    /// Closes the pane, asking first on the last tab on a worktree.
    fn pane_close_button(&self, pane: &PaneView, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = pane.id;
        let keys = {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            shortcut(&keymap, &ClosePane)
        };
        let label = match keys {
            Some(keys) => format!("Close {} ({keys})", pane.name),
            None => format!("Close {}", pane.name),
        };
        self.head_button("pane-close", id, assets::CLOSE)
            .aria_label(label)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.close_pane_id(id, window, cx);
            }))
    }

    /// The profiles a split can start, under the pane head (RF-PAN-18).
    fn render_split_menu(
        &self,
        pane: PaneId,
        orientation: Orientation,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = self.theme;
        let hover = t.term_raised;
        let options: Vec<AnyElement> = self
            .workspace
            .profiles()
            .iter()
            .enumerate()
            .map(|(at, profile)| {
                let hint = match (profile.id.as_str(), orientation) {
                    (CLAUDE_PROFILE, Orientation::Horizontal) => SPLIT_CLAUDE_KEYS,
                    (SHELL_PROFILE, Orientation::Horizontal) => SPLIT_RIGHT_KEYS,
                    (SHELL_PROFILE, Orientation::Vertical) => SPLIT_DOWN_KEYS,
                    _ => "",
                };
                let id = profile.id.clone();
                h_flex()
                    .id(("split-profile", at))
                    .role(Role::MenuItem)
                    .aria_label(format!("Start {}", profile.name))
                    .items_center()
                    .gap(px(12.0))
                    .px(px(10.0))
                    .py(px(5.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.split_pane(pane, orientation, false, Some(id.clone()), cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .text_color(t.term_text)
                            .child(profile.name.clone()),
                    )
                    .child(div().flex_none().text_color(t.term_faint).child(hint))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .id(("split-menu", pane.get()))
            .occlude()
            .role(Role::Menu)
            .aria_label("Start in the new pane")
            .absolute()
            .top(px(32.0))
            .right(px(8.0))
            .w(px(240.0))
            .p(px(4.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(t.term_strong)
            .bg(t.term_bg)
            .text_size(t.text(12.5))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu_closed = this
                    .split_menu
                    .take()
                    .map(|menu| (menu, std::time::Instant::now()));
                cx.notify();
            }))
            .children(options)
    }

    /// The context bar's box, without its contents.
    fn context_bar_frame(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = self.theme;
        v_flex()
            .id("context-bar")
            .key_context(CONTEXT_BAR_CONTEXT)
            .track_focus(&self.context_focus)
            .role(Role::Complementary)
            .aria_label("Context bar")
            .relative()
            .w(px(self.bar_widths.context_bar.unwrap_or(0.0)))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(t.line)
            .bg(t.panel)
            .when(self.ringed(Region::ContextBar), |bar| {
                bar.child(focus_ring(&t))
            })
            .children(self.render_bar_edge(Bar::Context, cx))
    }

    fn render_context_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let Some(tab) = self.workspace.selected_tab() else {
            // With no tab selected there is no branch to speak of.
            return self.context_bar_frame(cx).child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(t.text(12.5))
                    .text_color(t.faint)
                    .child(no_tab_label(&self.workspace)),
            );
        };
        let git = ContextGit::new(
            self.workspace.git_of(tab.id),
            tab.branch.as_deref(),
            tab.dir.as_str(),
        );
        let mono = self.fonts.mono.clone();
        let chip_label = if git.detached {
            format!("Detached at {}", git.head)
        } else {
            format!("Branch {}", git.head)
        };
        let hidden = git.hidden();
        let repository = git.repository;
        let repo_root = match self.workspace.git_of(tab.id) {
            Some(TabGit::Repository(facts)) => Some(facts.root.clone()),
            _ => None,
        };
        let changes_view = self.context_view == ContextView::Changes;
        let rows = git
            .files
            .into_iter()
            .enumerate()
            .map(|(at, file)| {
                let path = format!("{}{}", file.folder, file.name);
                let deleted = file.mark == FileMark::Deleted;
                let root = repo_root.clone();
                h_flex()
                    .id(("git-file", at))
                    .role(Role::ListItem)
                    .aria_label(file.label)
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.click_count() >= 2 {
                            this.open_changed_file(root.as_deref(), &path, deleted, cx);
                        }
                    }))
                    .items_center()
                    .gap(px(8.0))
                    .px(px(6.0))
                    .py(px(4.0))
                    .rounded(px(5.0))
                    .text_size(t.text(12.5))
                    .when_some(mono.clone(), |d, family| d.font_family(family))
                    .child(
                        div()
                            .flex_none()
                            .w(px(16.0))
                            .text_center()
                            .text_size(t.text(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.mark_color(file.mark))
                            .child(file.mark.letter()),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .child(
                                div()
                                    .flex_shrink_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(t.muted)
                                    .child(file.folder),
                            )
                            .child(div().flex_none().child(file.name)),
                    )
            })
            .collect::<Vec<_>>();
        self.context_bar_frame(cx)
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(t.line)
                    .child(
                        h_flex()
                            .id("branch-chip")
                            .role(Role::Status)
                            .aria_label(chip_label)
                            .flex_shrink_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(t.line)
                            .bg(t.bg)
                            .text_size(t.text(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .when_some(mono.clone(), |d, family| d.font_family(family))
                            .child(
                                svg()
                                    .flex_none()
                                    .path(assets::BRANCH)
                                    .size(px(13.0))
                                    .text_color(t.muted),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(git.head),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(t.text(12.0))
                            .text_color(t.faint)
                            .when_some(mono.clone(), |d, family| d.font_family(family))
                            .child(git.place),
                    )
                    .when(git.dirty, |row| {
                        row.child(
                            div()
                                .id("git-dirty")
                                .aria_label("Uncommitted changes")
                                .flex_none()
                                .px(px(6.0))
                                .py(px(1.0))
                                .rounded_full()
                                .border_1()
                                .border_color(t.line)
                                .text_size(t.text(11.0))
                                .text_color(t.muted)
                                .when_some(mono.clone(), |d, family| d.font_family(family))
                                .child("dirty"),
                        )
                    })
                    .when_some(git.ahead_behind, |row, text| {
                        row.child(
                            div()
                                .id("git-ahead-behind")
                                .aria_label(format!("Ahead and behind the upstream: {text}"))
                                .flex_none()
                                .whitespace_nowrap()
                                .text_size(t.text(12.0))
                                .text_color(t.muted)
                                .when_some(mono.clone(), |d, family| d.font_family(family))
                                .child(text),
                        )
                    }),
            )
            .child(self.render_context_tabs(repository.then_some(git.changed), cx))
            .when(!changes_view, |bar| bar.child(self.render_files(cx)))
            .when_some(git.note.filter(|_| changes_view), |bar, note| {
                bar.child(
                    div()
                        .flex_none()
                        .pt(px(10.0))
                        .px(px(12.0))
                        .pb(px(8.0))
                        .text_size(t.text(12.5))
                        .text_color(t.muted)
                        .child(note),
                )
            })
            .when(changes_view, |bar| {
                bar.child(
                    v_flex()
                        .id("git-changes")
                        .role(Role::List)
                        .aria_label("Changed files")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(px(6.0))
                        .pb(px(8.0))
                        .children(rows)
                        .when(hidden > 0, |list| {
                            list.child(
                                div()
                                    .px(px(6.0))
                                    .py(px(4.0))
                                    .text_size(t.text(12.0))
                                    .text_color(t.faint)
                                    .child(format!("and {hidden} more")),
                            )
                        }),
                )
            })
    }

    /// Opens a file of the change list, whose path is relative to the
    /// repository's root, not to the tab's folder (RF-CTX-03, RF-CTX-04).
    fn open_changed_file(
        &mut self,
        root: Option<&str>,
        path: &str,
        deleted: bool,
        cx: &mut Context<Self>,
    ) {
        if deleted {
            self.say(format!("{path} was deleted; there is nothing to open."), cx);
            return;
        }
        let Some(root) = root else { return };
        match join_relative(root, path) {
            Some(file) => self.open_file(file, cx),
            None => {
                self.say(format!("{path} is not inside the repository."), cx);
            }
        }
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let ws = &self.workspace;
        let hints = status::live_hints(&cx.key_bindings().borrow());
        let right = status::line(self.notice.as_ref(), &hints);
        h_flex()
            .id("status-bar")
            .role(Role::ContentInfo)
            .flex_none()
            .h(px(STATUS_BAR_HEIGHT))
            .items_center()
            .gap(px(16.0))
            .px(px(10.0))
            .border_t_1()
            .border_color(t.line)
            .bg(t.panel)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_size(t.text(11.5))
            .text_color(t.faint)
            .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
            .child(format!(
                "{} tabs · {} panes",
                ws.tab_count(),
                ws.pane_count()
            ))
            .child(format!("{} working", ws.working_panes()))
            .when_some(core_busy(self.link.shared_backlog()), |bar, text| {
                bar.child(
                    div()
                        .id("core-busy")
                        .role(Role::Status)
                        .text_color(t.ochre)
                        .child(text),
                )
            })
            .when_some(right, |bar, line| {
                bar.child(
                    h_flex().flex_1().min_w_0().justify_end().child(match line {
                        Line::Hints(text) => div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(text)
                            .into_any_element(),
                        Line::Notice { text, error } => div()
                            .id("notice")
                            .cursor_pointer()
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.clear_notice(cx);
                            }))
                            .role(Role::Status)
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(t.ochre)
                            .when(error, |d| d.font_weight(FontWeight::BOLD))
                            .child(text)
                            .into_any_element(),
                    }),
                )
            })
    }
}

/// What the context bar says with no tab selected: whether there is no
/// project at all, or projects whose tabs are all closed.
fn no_tab_label(workspace: &WorkspaceView) -> &'static str {
    if workspace.has_no_projects() {
        "No project"
    } else {
        "No tab is open"
    }
}

/// What the status bar says after a mute toggle.
fn mute_notice(muted: bool, what: &str) -> String {
    if muted {
        format!("{what} muted. The red mark and the counter stay.")
    } else {
        format!("{what} on again.")
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_title(window);
        self.keyboard_region = self.region(window, cx);
        self.keyboard_in_bar = region::bar_claim_holds(self.keyboard_in_bar, self.keyboard_region);
        // A drag that just ended sends its width or its share here.
        self.save_side_bars(cx);
        self.forget_ended_tab_drag(cx);
        self.finish_divider_drag(cx);
        self.bar_widths = sidebars::drawn(&self.side_bars, window.viewport_size().width.as_f32());
        let t = self.theme;
        v_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .child(self.watch_pointer(cx))
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::open_config_file))
            .on_action(cx.listener(Self::toggle_tab_bar))
            .on_action(cx.listener(Self::toggle_context_bar))
            .on_action(cx.listener(Self::toggle_review_mode))
            .on_drag_move(cx.listener(Self::on_bar_drag))
            .on_drag_move(cx.listener(Self::on_divider_drag))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::split_left))
            .on_action(cx.listener(Self::split_up))
            .on_action(cx.listener(Self::close_pane))
            .on_action(cx.listener(Self::focus_left))
            .on_action(cx.listener(Self::focus_right))
            .on_action(cx.listener(Self::focus_up))
            .on_action(cx.listener(Self::focus_down))
            .on_action(cx.listener(Self::grow_pane))
            .on_action(cx.listener(Self::shrink_pane))
            .on_action(cx.listener(Self::swap_left))
            .on_action(cx.listener(Self::swap_right))
            .on_action(cx.listener(Self::swap_up))
            .on_action(cx.listener(Self::swap_down))
            .on_action(cx.listener(Self::toggle_maximize))
            .on_action(cx.listener(Self::jump_to_needs_you))
            .on_action(cx.listener(Self::jump_to_done))
            .on_action(cx.listener(Self::toggle_attention_tray))
            .on_action(cx.listener(Self::toggle_tab_grouping))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::resume_past_session))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::new_default_tab))
            .on_action(cx.listener(Self::new_claude_tab))
            .on_action(cx.listener(Self::new_worktree_tab))
            .on_action(cx.listener(Self::worktree_prompt_confirm))
            .on_action(cx.listener(Self::worktree_prompt_cancel))
            .on_action(cx.listener(Self::worktree_keep))
            .on_action(cx.listener(Self::worktree_remove))
            .on_action(cx.listener(Self::worktree_remove_anyway))
            .on_action(cx.listener(Self::worktree_cancel))
            .on_action(cx.listener(Self::split_right_claude))
            .on_action(cx.listener(Self::relaunch_pane))
            .on_action(cx.listener(Self::open_pane_menu))
            .on_action(cx.listener(Self::terminate_pane))
            .on_action(cx.listener(Self::copy_session_id))
            .on_action(cx.listener(Self::toggle_project_fold))
            .on_action(cx.listener(Self::toggle_notifications_muted))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::reset_zoom))
            .on_action(cx.listener(Self::toggle_tab_muted))
            .on_action(cx.listener(Self::add_project))
            .on_action(cx.listener(Self::close_project))
            .on_action(cx.listener(Self::confirm_accept))
            .on_action(cx.listener(Self::confirm_cancel))
            .on_action(cx.listener(Self::rename_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::move_tab_up))
            .on_action(cx.listener(Self::move_tab_down))
            .on_action(cx.listener(Self::rename_confirm))
            .on_action(cx.listener(Self::rename_cancel))
            .on_action(cx.listener(Self::toggle_command_palette))
            .on_action(cx.listener(Self::toggle_switcher))
            .on_action(cx.listener(Self::palette_up))
            .on_action(cx.listener(Self::palette_down))
            .on_action(cx.listener(Self::palette_confirm))
            .on_action(cx.listener(Self::palette_close))
            .on_action(cx.listener(Self::focus_next_region))
            .on_action(cx.listener(Self::focus_previous_region))
            .on_action(cx.listener(Self::tab_bar_up))
            .on_action(cx.listener(Self::tab_bar_down))
            .on_action(cx.listener(Self::tab_bar_select))
            .on_action(cx.listener(Self::tab_bar_leave))
            .on_action(cx.listener(Self::dismiss_notice))
            .on_action(cx.listener(Self::context_bar_leave))
            .on_action(cx.listener(Self::divider_left))
            .on_action(cx.listener(Self::divider_right))
            .on_action(cx.listener(Self::divider_up))
            .on_action(cx.listener(Self::divider_down))
            .on_action(cx.listener(Self::divider_next))
            .on_action(cx.listener(Self::divider_previous))
            .on_action(cx.listener(Self::divider_leave))
            .on_action(cx.listener(Self::show_files))
            .on_action(cx.listener(Self::show_changes))
            .on_action(cx.listener(Self::refresh_files))
            .on_action(cx.listener(Self::file_tree_up))
            .on_action(cx.listener(Self::file_tree_down))
            .on_action(cx.listener(Self::file_tree_collapse))
            .on_action(cx.listener(Self::file_tree_expand))
            .on_action(cx.listener(Self::file_tree_open))
            .on_action(cx.listener(Self::file_tree_escape))
            .on_action(cx.listener(Self::show_whats_new))
            .on_action(cx.listener(Self::install_update))
            .on_action(cx.listener(|this, _: &GoToTab1, w, cx| this.go_to_tab(1, w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab2, w, cx| this.go_to_tab(2, w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab3, w, cx| this.go_to_tab(3, w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab4, w, cx| this.go_to_tab(4, w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab5, w, cx| this.go_to_tab(5, w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab6, w, cx| this.go_to_tab(6, w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab7, w, cx| this.go_to_tab(7, w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab8, w, cx| this.go_to_tab(8, w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab9, w, cx| this.go_to_tab(9, w, cx)))
            .relative()
            .size_full()
            .bg(t.panel)
            .text_color(t.text)
            .text_size(t.text(14.0))
            .when_some(self.fonts.ui.clone(), |d, family| d.font_family(family))
            .child(self.render_title_bar(window, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.bar_shown(Bar::Tab), |row| {
                        row.child(self.render_tab_bar(cx))
                    })
                    .child(self.render_panes(cx))
                    .when(self.bar_shown(Bar::Context), |row| {
                        row.child(self.render_context_bar(cx))
                    }),
            )
            .child(self.render_status_bar(cx))
            .children(self.render_tray(cx))
            .children(self.render_palette(cx))
            .children(self.render_worktree_prompt(cx))
            .children(self.render_worktree_close(cx))
            .children(self.render_menu(cx))
            .children(self.render_whats_new(cx))
            .children(self.render_confirm(cx))
    }
}

/// The line between two tab rows where a dragged tab would land
/// (RF-ESP-16): indigo like the other drag marks, never red (RN-01).
fn drop_line(above: bool, t: &Theme) -> Div {
    let line = div()
        .absolute()
        .left(px(4.0))
        .right(px(4.0))
        .h(px(2.0))
        .rounded(px(1.0))
        .bg(t.ai);
    if above {
        line.top(px(-2.0))
    } else {
        line.bottom(px(-2.0))
    }
}

/// The indigo ring drawn over a bar that holds the keyboard (RF-NAV-08);
/// the bar must be `relative`.
fn focus_ring(t: &Theme) -> Div {
    div()
        .absolute()
        .inset_0()
        .border_2()
        .border_color(t.focus_ring())
}

/// A 30 px square button holding a 16 px icon, as the prototype's `.icon-btn`.
fn icon_button(id: &'static str, icon: &'static str, t: &Theme) -> Stateful<Div> {
    let hover = t.hover;
    div()
        .id(id)
        .occlude()
        .role(Role::Button)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(30.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(svg().path(icon).size(px(16.0)).text_color(t.muted))
}

/// The key beside a dialog button's label. In the label's family: the mono
/// family's metrics drew it below the label's line.
fn key_hint(keys: &'static str, t: &Theme) -> Div {
    div()
        .flex_none()
        .text_size(t.text(11.5))
        .text_color(t.faint)
        .child(keys)
}

/// The state glyph in its 14 px slot. The shape tells the state, the colour
/// only reinforces it.
fn glyph(state: AgentState, t: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(14.0))
        .child(
            svg()
                .path(state_icon(state))
                .size(px(12.0))
                .text_color(t.state_color(state)),
        )
}

/// The user's home folder from the environment, if set.
fn home_folder() -> Option<String> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(var).ok().filter(|home| !home.is_empty())
}

fn toggled(on: bool) -> Toggled {
    if on { Toggled::True } else { Toggled::False }
}

/// The wall clock in milliseconds since the Unix epoch, for how long ago a
/// past session changed.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The catalog command behind each mouse control above, so every one has a
/// palette row (RF-NAV-02). A new clickable control adds its command here.
/// Tab rows are reached through the switcher; the split menu's profiles
/// through `profile_splits`; the pane and terminal menus' items each name
/// their command in `PaneAction::command`.
#[cfg(test)]
const MOUSE_COMMANDS: &[(&str, &str)] = &[
    ("theme button", "theme.toggle"),
    ("tab bar button", "tab_bar.toggle"),
    ("context bar button", "context_bar.toggle"),
    ("needs-you counter", "agent.tray"),
    ("attention tray rows", "agent.tray"),
    ("quit dialog buttons", "app.quit"),
    ("palette button", "palette.open"),
    ("project head", "project.fold"),
    ("add project button", "project.add"),
    ("empty state's add project button", "project.add"),
    ("project head close button", "project.close"),
    ("project head new tab button", "tab.new"),
    ("tab bar head new tab button", "tab.new_default"),
    ("close project dialog buttons", "project.close"),
    ("close tab dialog buttons", "tab.close"),
    ("close pane dialog buttons", "pane.close"),
    ("relaunch button", "pane.relaunch"),
    ("pane head close button", "pane.close"),
    ("pane actions button", "pane.actions"),
    ("pane head right click", "pane.actions"),
    ("tab row", "switcher.open"),
    ("tab row double-click", "tab.rename"),
    ("tab row drag", "tab.move_up"),
    ("tab row drag", "tab.move_down"),
    ("tab row close button", "tab.close"),
    ("worktree prompt buttons", "tab.new_worktree"),
    ("worktree question buttons", "pane.close"),
    ("context bar's Changes tab", "changes.show"),
    ("context bar's Files tab", "files.show"),
    ("file tree rows", "files.show"),
    ("changed file double-click", "files.show"),
];

#[cfg(test)]
mod tests {
    // Not `super::*`: the gpui glob there would shadow `#[test]`.
    use super::{MOUSE_COMMANDS, no_tab_label};
    use crate::catalog::CATALOG;
    use crate::model::WorkspaceView;
    use yien_proto::{ProjectId, ProjectInfo, WorkspaceSnapshot};

    #[test]
    fn the_context_bar_says_no_project_only_when_there_is_none() {
        assert_eq!(no_tab_label(&WorkspaceView::default()), "No project");
        let tabs_closed = WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![ProjectInfo {
                id: ProjectId::new(1),
                name: "yien".to_owned(),
                root: "C:/Code/yien".to_owned(),
                tabs: vec![],
                collapsed: false,
            }],
            ..WorkspaceSnapshot::default()
        });
        assert_eq!(no_tab_label(&tabs_closed), "No tab is open");
    }

    #[test]
    fn rf_nav_02_every_mouse_control_has_a_palette_command() {
        for (control, id) in MOUSE_COMMANDS {
            assert!(
                CATALOG.iter().any(|spec| spec.id == *id),
                "the {control} has no command {id}"
            );
        }
    }
}
