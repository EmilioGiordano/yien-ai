//! The window's side of the test control pipe (ADR-21): the requests the
//! binary hands over, and the pure parts of answering them — the state dump,
//! which command an id runs, which key a name presses.

use std::time::Instant;

use gpui_kit::Keystroke;
use tokio::sync::mpsc;
use yien_proto::file_preview::{PreviewMode, is_markdown_path};
use yien_proto::frame::Frame;
use yien_proto::test_control::{
    CommandArgs, FileScroll, PaneState, ProjectState, Reply, Request, SplitWay, TabState,
    VisibleRows,
};
use yien_proto::{Orientation, PaneId};

use crate::agent_badge::{AgentBadge, Badge, ShellBadge};
use crate::catalog::spec;
use crate::grid::named_key;
use crate::model::{AgentState, TabMark, WorkspaceView};
use crate::palette::Target;
use crate::pane_head::{pane_badge, program_tag};

/// One request from the test control pipe and where its reply goes.
///
/// ```
/// let (reply, answer) = std::sync::mpsc::channel();
/// let call = yien_ui::TestCall {
///     request: yien_proto::test_control::Request::State { text: false },
///     reply,
///     deadline: std::time::Instant::now() + std::time::Duration::from_secs(5),
/// };
/// let _ = call.reply.send(yien_proto::test_control::Reply::done());
/// assert!(answer.recv().unwrap().ok);
/// ```
#[derive(Debug)]
pub struct TestCall {
    /// What the script asks.
    pub request: Request,
    /// Where the window sends its one reply.
    pub reply: std::sync::mpsc::Sender<Reply>,
    /// After this the pipe has stopped waiting, so the window must not act:
    /// a script that retries would otherwise press a key twice.
    pub deadline: Instant,
}

impl TestCall {
    /// Whether the call's deadline has passed at `now`.
    ///
    /// ```
    /// use std::time::{Duration, Instant};
    /// let (reply, _answer) = std::sync::mpsc::channel();
    /// let now = Instant::now();
    /// let call = yien_ui::TestCall {
    ///     request: yien_proto::test_control::Request::State { text: false },
    ///     reply,
    ///     deadline: now,
    /// };
    /// assert!(call.expired(now));
    /// assert!(!call.expired(now - Duration::from_millis(1)));
    /// ```
    pub fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }
}

/// The reply to a call that came after its deadline: nothing was done.
pub(crate) fn expired_reply() -> Reply {
    Reply::error("expired: the request reached the window after its deadline and was not run")
}

/// The requests the window answers while `--test-control` is on.
///
/// ```
/// let (calls, receiver) = tokio::sync::mpsc::channel(8);
/// let control = yien_ui::TestControl::new(receiver);
/// # drop((calls, control));
/// ```
#[derive(Debug)]
pub struct TestControl {
    pub(crate) calls: mpsc::Receiver<TestCall>,
}

impl TestControl {
    /// Wraps the receiving end the binary feeds from the pipe.
    ///
    /// ```
    /// let (_calls, receiver) = tokio::sync::mpsc::channel(1);
    /// let _control = yien_ui::TestControl::new(receiver);
    /// ```
    pub fn new(calls: mpsc::Receiver<TestCall>) -> Self {
        Self { calls }
    }
}

/// The command ids that take arguments: the palette's rows that are not in
/// the catalog, made from the profiles and the workspace.
pub(crate) const ARGUMENT_COMMANDS: &[&str] = &[
    "tab.new_with",
    "pane.split_with",
    "pane.focus",
    "tab.select",
    "project.select",
    "file.open",
    "files.menu",
    "files.menu_pick",
];

/// What a command id runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Run {
    /// The catalog entry with this id: its action, dispatched as the palette
    /// does.
    Catalog(&'static str),
    /// A palette row that takes arguments.
    Target(Target),
    /// Open the Files view's context menu on the drawn entry `path`, or on
    /// the tab's folder itself for `.` (RF-CTX-01).
    FilesMenu { path: String },
    /// Run the open Files menu's item with this label, typing `name` into
    /// the name field it opens and confirming it.
    FilesMenuPick { item: String, name: Option<String> },
}

/// Which command `id` runs with `args`, checked against what the window
/// knows: the catalog's ids take no arguments, so a request carrying any
/// is refused; the palette's argument rows must have the ones they read,
/// and the profile must be one the palette lists, the pane, tab or project
/// must exist.
pub(crate) fn resolve_command(
    id: &str,
    args: &CommandArgs,
    ws: &WorkspaceView,
) -> Result<Run, String> {
    if let Some(spec) = spec(id) {
        if *args != CommandArgs::default() {
            return Err(format!("{id} takes no args"));
        }
        return Ok(Run::Catalog(spec.id));
    }
    let profile = || {
        let wanted = args
            .profile
            .clone()
            .ok_or_else(|| format!("{id} needs args.profile"))?;
        if ws.profiles().iter().any(|p| p.id == wanted) {
            Ok(wanted)
        } else {
            Err(format!("no profile {}", wanted.as_str()))
        }
    };
    let target = match id {
        "tab.new_with" => Target::NewTab(profile()?),
        "pane.split_with" => {
            let orientation = match args.direction {
                Some(SplitWay::Right) => Orientation::Horizontal,
                Some(SplitWay::Down) => Orientation::Vertical,
                None => return Err(format!("{id} needs args.direction: right or down")),
            };
            Target::Split(profile()?, orientation)
        }
        "pane.focus" => {
            let pane = args.pane.ok_or_else(|| format!("{id} needs args.pane"))?;
            if ws.tab_of(pane).is_none() {
                return Err(format!("no pane {}", pane.get()));
            }
            Target::Pane(pane)
        }
        "tab.select" => {
            let tab = args.tab.ok_or_else(|| format!("{id} needs args.tab"))?;
            if ws.tab(tab).is_none() {
                return Err(format!("no tab {}", tab.get()));
            }
            Target::Tab(tab)
        }
        "project.select" => {
            let project = args
                .project
                .ok_or_else(|| format!("{id} needs args.project"))?;
            if ws.project(project).is_none() {
                return Err(format!("no project {}", project.get()));
            }
            Target::Project(project)
        }
        "file.open" => {
            let path = args
                .path
                .clone()
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| format!("{id} needs args.path"))?;
            let tab = match args.tab {
                Some(tab) => {
                    if ws.tab(tab).is_none() {
                        return Err(format!("no tab {}", tab.get()));
                    }
                    tab
                }
                None => ws
                    .selected_tab()
                    .map(|t| t.id)
                    .ok_or_else(|| "no tab is selected".to_owned())?,
            };
            Target::OpenFile { tab, path }
        }
        "files.menu" => {
            let path = args
                .path
                .clone()
                .ok_or_else(|| format!("{id} needs args.path: a drawn row, or . for the folder"))?;
            if ws.selected_tab().is_none() {
                return Err("no tab is selected".to_owned());
            }
            return Ok(Run::FilesMenu { path });
        }
        "files.menu_pick" => {
            let item = args
                .item
                .clone()
                .ok_or_else(|| format!("{id} needs args.item: the label of a menu item"))?;
            return Ok(Run::FilesMenuPick {
                item,
                name: args.name.clone(),
            });
        }
        _ => {
            return Err(format!(
                "no command {id}: use a catalog id or one of {}",
                ARGUMENT_COMMANDS.join(", ")
            ));
        }
    };
    Ok(Run::Target(target))
}

/// The keystroke a key name presses, in gpui notation (`ctrl-c`, `enter`,
/// `shift-tab`). A key that would type a character is refused: characters
/// go through `text`, as typing them does.
pub(crate) fn parse_key(key: &str) -> Result<Keystroke, String> {
    let keystroke = Keystroke::parse(key).map_err(|_| format!("not a key: {key}"))?;
    let m = keystroke.modifiers;
    let named = named_key(&keystroke.key).is_some();
    if !named && !(m.control || m.alt || m.platform || m.function) {
        return Err(format!("{key} types a character; send it with text"));
    }
    Ok(keystroke)
}

/// What the state dump reads from a pane's terminal view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalFacts {
    /// Columns and rows of its last frame.
    pub(crate) size: (u16, u16),
    /// Millis since the UNIX epoch when the frame was taken, if any.
    pub(crate) frame_at_ms: Option<u64>,
    /// Its visible rows, when asked for.
    pub(crate) rows: Option<Vec<String>>,
    /// A file pane's scroll position.
    pub(crate) scroll: Option<FileScroll>,
}

impl TerminalFacts {
    pub(crate) fn of(frame: &Frame, with_text: bool, frame_at_ms: Option<u64>) -> Self {
        Self {
            size: (frame.size.cols, frame.size.rows),
            frame_at_ms,
            rows: with_text.then(|| (0..frame.lines.len()).map(|r| frame.line_text(r)).collect()),
            scroll: None,
        }
    }
}

/// The projects, tabs and panes as the window shows them. `keyboard` is the
/// pane whose terminal has the keyboard; `terminal` reads a pane's view.
pub(crate) fn workspace_state(
    ws: &WorkspaceView,
    keyboard: Option<PaneId>,
    terminal: &dyn Fn(PaneId) -> Option<TerminalFacts>,
) -> (Vec<ProjectState>, Vec<TabState>, Vec<PaneState>) {
    let drawn: Vec<PaneId> = ws
        .selected_tab()
        .map(|tab| tab.rects().into_iter().map(|(p, _)| p).collect())
        .unwrap_or_default();
    let mut projects = Vec::new();
    let mut tabs = Vec::new();
    let mut panes = Vec::new();
    for project in ws.projects() {
        projects.push(ProjectState {
            id: project.id,
            name: project.name.clone(),
            root: project.root.clone(),
            folded: ws.is_folded(project.id),
            tabs: project.tabs.clone(),
        });
        for tab in ws.tabs_of(project) {
            let state = ws.tab_state(tab);
            let ids = tab.layout.panes();
            tabs.push(TabState {
                id: tab.id,
                project: tab.project,
                name: tab.name.clone(),
                dir: tab.dir.clone(),
                branch: tab.branch.clone(),
                worktree: tab.worktree.is_some(),
                badge: ws.tab_badge(tab).map(|b| badge_key(b).to_owned()),
                state: state.label().to_owned(),
                mark: match ws.tab_mark(tab) {
                    TabMark::Shell => "shell".to_owned(),
                    TabMark::State(state) => state_key(state).to_owned(),
                },
                needs_you: state.needs_you(),
                muted: tab.muted,
                focused: tab.focused,
                maximized: tab.maximized,
                panes: ids.clone(),
            });
            for id in ids {
                let pane = ws.pane(id);
                let facts = terminal(id);
                let (kind, open_files, active_file, wrap, mode) = match pane.file_pane() {
                    Some(file) => (
                        "file".to_owned(),
                        Some(file.files.clone()),
                        file.active_file().map(str::to_owned),
                        Some(file.wrap),
                        Some(shown_mode(file).key().to_owned()),
                    ),
                    None => ("terminal".to_owned(), None, None, None, None),
                };
                panes.push(PaneState {
                    id,
                    tab: tab.id,
                    name: pane.name.clone(),
                    profile: pane.profile.clone(),
                    profile_id: pane.profile_id.as_str().to_owned(),
                    badge: pane_badge(&pane).map(|b| badge_key(b).to_owned()),
                    tag: program_tag(&pane).to_owned(),
                    state: pane.state.label().to_owned(),
                    signal_lost: pane.signal_lost,
                    running: pane.running,
                    exited: pane.exited,
                    exit_code: pane.exit_code,
                    cwd: pane.cwd.clone(),
                    focused: tab.focused == id,
                    keyboard: keyboard == Some(id),
                    visible: drawn.contains(&id),
                    size: facts.as_ref().map(|f| f.size),
                    frame_at_ms: facts.as_ref().and_then(|f| f.frame_at_ms),
                    scroll: facts.as_ref().and_then(|f| f.scroll),
                    rows: facts.and_then(|f| f.rows).map(VisibleRows::new),
                    kind,
                    open_files,
                    active_file,
                    wrap,
                    mode,
                });
            }
        }
    }
    (projects, tabs, panes)
}

/// How a file pane draws its shown file: a Markdown file in the pane's
/// mode, any other file as source.
fn shown_mode(file: &yien_proto::FilePaneInfo) -> PreviewMode {
    if file.active_file().is_some_and(is_markdown_path) {
        file.mode
    } else {
        PreviewMode::Source
    }
}

/// A badge as a snake case key, as `badge` carries it.
fn badge_key(badge: Badge) -> &'static str {
    match badge {
        Badge::Agent(AgentBadge::Claude) => "claude",
        Badge::Agent(AgentBadge::Codex) => "codex",
        Badge::Agent(AgentBadge::OpenCode) => "opencode",
        Badge::Shell(ShellBadge::PowerShell) => "powershell",
        Badge::Shell(ShellBadge::Cmd) => "cmd",
        Badge::Shell(ShellBadge::GitBash) => "git_bash",
        Badge::Shell(ShellBadge::Other) => "other",
    }
}

/// A state as a snake case key, as `mark` carries it.
fn state_key(state: AgentState) -> &'static str {
    match state {
        AgentState::NeedsYou => "needs_you",
        AgentState::Error => "error",
        AgentState::Done => "done",
        AgentState::Working => "working",
        AgentState::Idle => "idle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yien_proto::frame::{Cell, CellFlags, Rgb, Size};
    use yien_proto::{
        LayoutChild, LayoutNode, NeedsReason, PaneInfo, ProfileId, ProfileInfo, ProjectId,
        ProjectInfo, TabId, TabInfo, WorkspaceSnapshot,
    };

    fn pane_info(
        id: u64,
        tab: u64,
        profile: &str,
        name: &str,
        state: yien_proto::AgentState,
    ) -> PaneInfo {
        PaneInfo {
            id: PaneId::new(id),
            tab: TabId::new(tab),
            name: name.into(),
            profile: profile.into(),
            profile_id: ProfileId::new(profile),
            title: None,
            cwd: None,
            state,
            running: true,
            exit_code: None,
            session_id: None,
            needs_you_since: None,
            attention_since: None,
            question: None,
            signal_lost: false,
            runs_session: true,
            kind: yien_proto::PaneKind::Terminal,
        }
    }

    /// One project with a tab of a Claude pane beside a shell, and a second
    /// tab with a shell alone.
    fn workspace() -> WorkspaceView {
        let split = LayoutNode::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                LayoutChild {
                    node: LayoutNode::Pane(PaneId::new(3)),
                    flex: 0.5,
                },
                LayoutChild {
                    node: LayoutNode::Pane(PaneId::new(4)),
                    flex: 0.5,
                },
            ],
        };
        let tab = |id: u64, layout: LayoutNode, focused: u64| TabInfo {
            id: TabId::new(id),
            project: ProjectId::new(1),
            name: format!("Tab {id}"),
            dir: "C:/work".into(),
            branch: Some("main".into()),
            layout,
            focused: PaneId::new(focused),
            maximized: None,
            muted: false,
            worktree: None,
        };
        WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![ProjectInfo {
                id: ProjectId::new(1),
                name: "work".into(),
                root: "C:/work".into(),
                tabs: vec![TabId::new(2), TabId::new(5)],
                collapsed: false,
            }],
            tabs: vec![
                tab(2, split, 4),
                tab(5, LayoutNode::Pane(PaneId::new(6)), 6),
            ],
            panes: vec![
                pane_info(
                    3,
                    2,
                    "claude",
                    "claude",
                    yien_proto::AgentState::NeedsYou(NeedsReason::Permission),
                ),
                pane_info(4, 2, "shell", "pwsh", yien_proto::AgentState::Idle),
                pane_info(6, 5, "shell", "pwsh", yien_proto::AgentState::Idle),
            ],
            active_tab: Some(TabId::new(2)),
            profiles: vec![
                ProfileInfo {
                    id: ProfileId::new("shell"),
                    name: "Shell".into(),
                },
                ProfileInfo {
                    id: ProfileId::new("claude"),
                    name: "Claude Code".into(),
                },
            ],
            ..WorkspaceSnapshot::default()
        })
    }

    fn frame(rows: &[&str], cols: u16) -> Frame {
        let cell = |ch| Cell {
            ch,
            zerowidth: Vec::new(),
            fg: Rgb::default(),
            bg: Rgb::default(),
            flags: CellFlags::EMPTY,
        };
        Frame {
            size: Size {
                cols,
                rows: rows.len() as u16,
            },
            lines: rows
                .iter()
                .map(|r| {
                    let mut line: Vec<Cell> = r.chars().map(cell).collect();
                    line.resize(cols as usize, cell(' '));
                    line
                })
                .collect(),
            cursor: None,
            display_offset: 0,
            history_len: 0,
            background: Rgb::default(),
            cursor_color: Rgb::default(),
            modes: Default::default(),
        }
    }

    #[test]
    fn adr_21_state_lists_projects_tabs_and_panes_in_tab_bar_order() {
        let ws = workspace();
        let (projects, tabs, panes) = workspace_state(&ws, Some(PaneId::new(4)), &|_| None);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].tabs, vec![TabId::new(2), TabId::new(5)]);
        let ids: Vec<u64> = tabs.iter().map(|t| t.id.get()).collect();
        assert_eq!(ids, vec![2, 5]);
        let ids: Vec<u64> = panes.iter().map(|p| p.id.get()).collect();
        assert_eq!(ids, vec![3, 4, 6]);
        assert_eq!(tabs[0].panes, vec![PaneId::new(3), PaneId::new(4)]);
        assert_eq!(tabs[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn rf_pan_12_each_pane_and_tab_carries_its_badge() {
        let ws = workspace();
        let (_, tabs, panes) = workspace_state(&ws, Some(PaneId::new(4)), &|_| None);
        let badge = |p: &PaneState| p.badge.clone();
        assert_eq!(
            badge(&panes[0]).as_deref(),
            Some("claude"),
            "the claude pane"
        );
        assert_eq!(
            badge(&panes[1]).as_deref(),
            Some("powershell"),
            "the shell pane"
        );
        assert_eq!(
            badge(&panes[2]).as_deref(),
            Some("powershell"),
            "the other shell pane"
        );
        // The tab's badge is its most urgent pane's: Claude's in the split tab,
        // the shell's in the shells-only tab.
        assert_eq!(tabs[0].badge.as_deref(), Some("claude"));
        assert_eq!(tabs[1].badge.as_deref(), Some("powershell"));
    }

    /// One project with one cmd pane.
    fn cmd_workspace() -> WorkspaceView {
        WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![ProjectInfo {
                id: ProjectId::new(1),
                name: "work".into(),
                root: "C:/work".into(),
                tabs: vec![TabId::new(2)],
                collapsed: false,
            }],
            tabs: vec![TabInfo {
                id: TabId::new(2),
                project: ProjectId::new(1),
                name: "Tab 2".into(),
                dir: "C:/work".into(),
                branch: None,
                layout: LayoutNode::Pane(PaneId::new(3)),
                focused: PaneId::new(3),
                maximized: None,
                muted: false,
                worktree: None,
            }],
            panes: vec![PaneInfo {
                profile: "Command Prompt".into(),
                ..pane_info(3, 2, "shell:cmd", "cmd", yien_proto::AgentState::Idle)
            }],
            active_tab: Some(TabId::new(2)),
            ..WorkspaceSnapshot::default()
        })
    }

    #[test]
    fn rf_pan_12_a_cmd_pane_and_its_tab_carry_the_cmd_badge() {
        let ws = cmd_workspace();
        let (_, tabs, panes) = workspace_state(&ws, None, &|_| None);
        assert_eq!(panes[0].badge.as_deref(), Some("cmd"));
        assert_eq!(tabs[0].badge.as_deref(), Some("cmd"));
        assert_eq!(tabs[0].mark, "shell");
    }

    #[test]
    fn rn_01_a_tab_with_a_waiting_agent_is_marked_needs_you_and_a_tab_of_shells_has_no_mark() {
        let ws = workspace();
        let (_, tabs, _) = workspace_state(&ws, None, &|_| None);
        assert_eq!(tabs[0].mark, "needs_you");
        assert_eq!(tabs[0].state, "Needs you");
        assert!(tabs[0].needs_you);
        assert_eq!(tabs[1].mark, "shell");
        assert_eq!(tabs[1].state, "Idle");
        assert!(!tabs[1].needs_you);
    }

    #[test]
    fn adr_21_panes_carry_their_head_focus_keyboard_and_visibility() {
        let ws = workspace();
        let (_, _, panes) = workspace_state(&ws, Some(PaneId::new(4)), &|_| None);
        let claude = &panes[0];
        assert_eq!(
            (
                claude.name.as_str(),
                claude.profile_id.as_str(),
                claude.tag.as_str()
            ),
            ("claude", "claude", "claude")
        );
        assert_eq!(claude.state, "Needs you");
        assert!(!claude.focused && !claude.keyboard && claude.visible);
        let shell = &panes[1];
        assert_eq!(shell.tag, "pwsh");
        assert!(shell.focused && shell.keyboard && shell.visible);
        let hidden = &panes[2];
        assert!(hidden.focused && !hidden.keyboard && !hidden.visible);
    }

    #[test]
    fn adr_21_visible_rows_come_only_from_a_frame_and_only_when_asked() {
        let ws = workspace();
        let shown = frame(&["PS C:\\work> claude", ""], 30);
        let with_text =
            |p: PaneId| (p == PaneId::new(4)).then(|| TerminalFacts::of(&shown, true, Some(42)));
        let (_, _, panes) = workspace_state(&ws, None, &with_text);
        assert_eq!(panes[1].size, Some((30, 2)));
        assert_eq!(panes[1].frame_at_ms, Some(42));
        assert_eq!(panes[0].frame_at_ms, None);
        assert_eq!(
            panes[1].rows.as_ref().map(|r| r.as_slice().to_vec()),
            Some(vec!["PS C:\\work> claude".to_owned(), String::new()])
        );
        assert!(panes[0].rows.is_none() && panes[0].size.is_none());

        let without =
            |p: PaneId| (p == PaneId::new(4)).then(|| TerminalFacts::of(&shown, false, None));
        let (_, _, panes) = workspace_state(&ws, None, &without);
        assert_eq!(panes[1].size, Some((30, 2)));
        assert_eq!(panes[1].frame_at_ms, None);
        assert!(panes[1].rows.is_none());
    }

    #[test]
    fn adr_21_catalog_ids_refuse_args_they_would_ignore() {
        let ws = workspace();
        for id in CATALOG_ID_SAMPLE {
            for (what, args) in ARG_VARIANTS {
                assert_eq!(
                    resolve_command(id, &args(), &ws),
                    Err(format!("{id} takes no args")),
                    "{id} with {what}"
                );
            }
        }
        // The same ids from the existing test still resolve without args.
        for id in ["tab.new_claude", "app.quit"] {
            assert_eq!(
                resolve_command(id, &CommandArgs::default(), &ws),
                Ok(Run::Catalog(spec(id).unwrap().id))
            );
        }
    }

    const CATALOG_ID_SAMPLE: &[&str] = &[
        "tab.new",
        "tab.new_claude",
        "app.quit",
        "pane.split_right",
        "pane.close",
        "terminal.copy",
        "agent.next_needs_you",
    ];

    /// A name for one args-carrying case, and how to build it.
    type ArgCase = (&'static str, fn() -> CommandArgs);

    /// Each way a request can carry args: one arg set, every field set.
    const ARG_VARIANTS: &[ArgCase] = &[
        ("profile", || CommandArgs {
            profile: Some(ProfileId::new("claude")),
            ..CommandArgs::default()
        }),
        ("direction", || CommandArgs {
            direction: Some(SplitWay::Down),
            ..CommandArgs::default()
        }),
        ("pane", || CommandArgs {
            pane: Some(PaneId::new(3)),
            ..CommandArgs::default()
        }),
        ("tab", || CommandArgs {
            tab: Some(TabId::new(2)),
            ..CommandArgs::default()
        }),
        ("project", || CommandArgs {
            project: Some(ProjectId::new(1)),
            ..CommandArgs::default()
        }),
        ("path", || CommandArgs {
            path: Some("src/main.rs".into()),
            ..CommandArgs::default()
        }),
        ("everything", || CommandArgs {
            profile: Some(ProfileId::new("shell")),
            direction: Some(SplitWay::Right),
            pane: Some(PaneId::new(3)),
            tab: Some(TabId::new(2)),
            project: Some(ProjectId::new(1)),
            path: Some("src/main.rs".into()),
            item: Some("Rename…".into()),
            name: Some("lib.rs".into()),
        }),
    ];

    #[test]
    fn adr_19_catalog_ids_run_their_catalog_entry() {
        let ws = workspace();
        for id in [
            "pane.split_right",
            "pane.close",
            "tab.new_claude",
            "terminal.copy",
        ] {
            assert_eq!(
                resolve_command(id, &CommandArgs::default(), &ws),
                Ok(Run::Catalog(spec(id).unwrap().id))
            );
        }
    }

    #[test]
    fn adr_21_argument_ids_keep_ignoring_the_args_they_do_not_read() {
        let ws = workspace();
        // tab.new_with reads only profile: a stray pane beside it still runs.
        let stray = CommandArgs {
            profile: Some(ProfileId::new("claude")),
            pane: Some(PaneId::new(4)),
            ..CommandArgs::default()
        };
        assert_eq!(
            resolve_command("tab.new_with", &stray, &ws),
            Ok(Run::Target(Target::NewTab(ProfileId::new("claude"))))
        );
    }

    #[test]
    fn adr_21_argument_commands_resolve_to_the_palette_rows() {
        let ws = workspace();
        let args = |f: fn(&mut CommandArgs)| {
            let mut a = CommandArgs::default();
            f(&mut a);
            a
        };
        assert_eq!(
            resolve_command(
                "tab.new_with",
                &args(|a| a.profile = Some(ProfileId::new("claude"))),
                &ws
            ),
            Ok(Run::Target(Target::NewTab(ProfileId::new("claude"))))
        );
        assert_eq!(
            resolve_command(
                "pane.split_with",
                &args(|a| {
                    a.profile = Some(ProfileId::new("shell"));
                    a.direction = Some(SplitWay::Down);
                }),
                &ws
            ),
            Ok(Run::Target(Target::Split(
                ProfileId::new("shell"),
                Orientation::Vertical
            )))
        );
        assert_eq!(
            resolve_command("pane.focus", &args(|a| a.pane = Some(PaneId::new(6))), &ws),
            Ok(Run::Target(Target::Pane(PaneId::new(6))))
        );
        assert_eq!(
            resolve_command("tab.select", &args(|a| a.tab = Some(TabId::new(5))), &ws),
            Ok(Run::Target(Target::Tab(TabId::new(5))))
        );
        assert_eq!(
            resolve_command(
                "project.select",
                &args(|a| a.project = Some(ProjectId::new(1))),
                &ws
            ),
            Ok(Run::Target(Target::Project(ProjectId::new(1))))
        );
    }

    #[test]
    fn rf_ctx_01_the_file_menu_opens_on_a_row_and_picks_an_item_by_label() {
        let ws = workspace();
        let args = |path: Option<&str>, item: Option<&str>, name: Option<&str>| CommandArgs {
            path: path.map(str::to_owned),
            item: item.map(str::to_owned),
            name: name.map(str::to_owned),
            ..CommandArgs::default()
        };
        assert_eq!(
            resolve_command("files.menu", &args(Some("src/main.rs"), None, None), &ws),
            Ok(Run::FilesMenu {
                path: "src/main.rs".into()
            })
        );
        assert_eq!(
            resolve_command("files.menu", &args(Some("."), None, None), &ws),
            Ok(Run::FilesMenu { path: ".".into() })
        );
        assert_eq!(
            resolve_command(
                "files.menu_pick",
                &args(None, Some("New File…"), Some("notes.md")),
                &ws
            ),
            Ok(Run::FilesMenuPick {
                item: "New File…".into(),
                name: Some("notes.md".into())
            })
        );
        assert!(resolve_command("files.menu", &args(None, None, None), &ws).is_err());
        assert!(resolve_command("files.menu_pick", &args(None, None, None), &ws).is_err());
        let empty = WorkspaceView::from_snapshot(&Default::default());
        assert!(resolve_command("files.menu", &args(Some("a"), None, None), &empty).is_err());
    }

    #[test]
    fn adr_21_unknown_ids_missing_args_and_unknown_things_are_refused() {
        let ws = workspace();
        let none = CommandArgs::default();
        for id in ARGUMENT_COMMANDS {
            assert!(spec(id).is_none(), "{id} must not shadow a catalog id");
            assert!(resolve_command(id, &none, &ws).is_err(), "{id}");
        }
        assert!(resolve_command("disk.format", &none, &ws).is_err());
        let unknown = CommandArgs {
            profile: Some(ProfileId::new("nope")),
            direction: Some(SplitWay::Right),
            pane: Some(PaneId::new(99)),
            tab: Some(TabId::new(99)),
            project: Some(ProjectId::new(99)),
            path: None,
            item: None,
            name: None,
        };
        for id in ARGUMENT_COMMANDS {
            assert!(resolve_command(id, &unknown, &ws).is_err(), "{id}");
        }
        let no_direction = CommandArgs {
            profile: Some(ProfileId::new("shell")),
            ..CommandArgs::default()
        };
        assert!(resolve_command("pane.split_with", &no_direction, &ws).is_err());
    }

    #[test]
    fn adr_21_an_expired_call_is_refused_and_a_live_one_is_not() {
        let (reply, _answer) = std::sync::mpsc::channel();
        let now = Instant::now();
        let call = TestCall {
            request: Request::Key {
                pane: None,
                key: "ctrl-c".into(),
            },
            reply,
            deadline: now + std::time::Duration::from_millis(50),
        };
        assert!(!call.expired(now));
        assert!(call.expired(call.deadline));
        assert!(call.expired(now + std::time::Duration::from_secs(1)));
        let refused = expired_reply();
        assert!(!refused.ok);
        assert!(refused.error.unwrap().starts_with("expired"));
    }

    #[test]
    fn rf_ctx_02_test_control_exposes_file_pane_state_and_resolves_file_open_and_wrap() {
        let mut ws = workspace();
        ws.pane_updated(&PaneInfo {
            name: "main.rs".into(),
            profile: "Files".into(),
            profile_id: ProfileId::new("file"),
            running: false,
            runs_session: false,
            kind: yien_proto::PaneKind::File(yien_proto::FilePaneInfo {
                files: vec!["src/main.rs".into(), "Cargo.toml".into()],
                active: Some(0),
                wrap: true,
                mode: PreviewMode::Rendered,
            }),
            ..pane_info(4, 2, "file", "main.rs", yien_proto::AgentState::Idle)
        });
        let scroll = FileScroll {
            top_line: 40,
            rows: 30,
            total_lines: 300,
        };
        let facts = |p: PaneId| {
            (p == PaneId::new(4)).then_some(TerminalFacts {
                size: (80, 30),
                frame_at_ms: None,
                rows: None,
                scroll: Some(scroll),
            })
        };
        let (_, _, panes) = workspace_state(&ws, Some(PaneId::new(4)), &facts);
        let file_pane = panes.iter().find(|p| p.id == PaneId::new(4)).unwrap();
        assert_eq!(file_pane.kind, "file");
        assert_eq!(file_pane.scroll, Some(scroll));
        assert_eq!(
            file_pane.open_files,
            Some(vec!["src/main.rs".into(), "Cargo.toml".into()])
        );
        assert_eq!(file_pane.active_file.as_deref(), Some("src/main.rs"));
        assert_eq!(file_pane.wrap, Some(true));
        // A Rust file shows its source whatever the pane's Markdown mode.
        assert_eq!(file_pane.mode.as_deref(), Some("source"));

        assert_eq!(
            resolve_command(
                "file.open",
                &CommandArgs {
                    path: Some("src/lib.rs".into()),
                    ..CommandArgs::default()
                },
                &ws
            ),
            Ok(Run::Target(Target::OpenFile {
                tab: TabId::new(2),
                path: "src/lib.rs".into(),
            }))
        );
        assert_eq!(
            resolve_command("file.toggle_wrap", &CommandArgs::default(), &ws),
            Ok(Run::Catalog("file.toggle_wrap"))
        );
        assert_eq!(
            resolve_command("file.toggle_rendered", &CommandArgs::default(), &ws),
            Ok(Run::Catalog("file.toggle_rendered"))
        );
    }

    #[test]
    fn rf_ctx_02_test_control_reports_how_a_markdown_file_is_drawn() {
        let mode_of = |mode: PreviewMode| {
            let mut ws = workspace();
            ws.pane_updated(&PaneInfo {
                name: "README.md".into(),
                profile: "Files".into(),
                profile_id: ProfileId::new("file"),
                running: false,
                runs_session: false,
                kind: yien_proto::PaneKind::File(yien_proto::FilePaneInfo {
                    files: vec!["README.md".into()],
                    active: Some(0),
                    wrap: false,
                    mode,
                }),
                ..pane_info(4, 2, "file", "README.md", yien_proto::AgentState::Idle)
            });
            let (_, _, panes) = workspace_state(&ws, None, &|_| None);
            panes
                .into_iter()
                .find(|p| p.id == PaneId::new(4))
                .and_then(|p| p.mode)
        };
        assert_eq!(mode_of(PreviewMode::Rendered).as_deref(), Some("rendered"));
        assert_eq!(mode_of(PreviewMode::Source).as_deref(), Some("source"));
    }

    #[test]
    fn adr_21_keys_parse_in_gpui_notation_and_characters_are_refused() {
        let ctrl_c = parse_key("ctrl-c").unwrap();
        assert!(ctrl_c.modifiers.control);
        assert_eq!(ctrl_c.key, "c");
        for named in [
            "enter",
            "escape",
            "up",
            "f6",
            "shift-tab",
            "alt-left",
            "backspace",
        ] {
            assert!(parse_key(named).is_ok(), "{named}");
        }
        for typed in ["a", "space", "shift-a", "1", "-"] {
            assert!(parse_key(typed).is_err(), "{typed}");
        }
    }
}
