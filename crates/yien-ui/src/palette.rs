//! The command palette, the switcher and the past sessions list as plain
//! data (RF-NAV-01 to 04, RF-SES-07): the rows each one offers, the query,
//! the ranked matches and the selection. The window draws it and runs what
//! it returns.

use std::ops::Range;

use yien_proto::{AgentSession, Orientation, PaneId, ProfileId, ProfileInfo, ProjectId, TabId};

use crate::catalog::{CATALOG, profile_splits, profile_tabs};
use crate::matcher::{Match, rank};
use crate::model::{AgentState, WorkspaceView};

/// Which list the palette opened with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// "Execute a command": the catalog (RF-NAV-01).
    Commands,
    /// "Go to": projects, tabs and panes (RF-NAV-04).
    Switcher,
    /// "Resume a past session" of one project (RF-SES-07).
    Sessions,
}

/// What running a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// Dispatch the catalog command at this index.
    Command(usize),
    /// Split the focused pane and start this profile in the new one.
    Split(ProfileId, Orientation),
    /// Open a tab running this profile.
    NewTab(ProfileId),
    /// Select the project's most recent tab.
    Project(ProjectId),
    /// Select the tab.
    Tab(TabId),
    /// Focus the pane, selecting its tab.
    Pane(PaneId),
    /// Resume a past agent session in a new pane.
    Resume {
        project: ProjectId,
        profile: ProfileId,
        session_id: String,
    },
}

/// One row the palette can show. Its `Debug` leaves the text out: a past
/// session's row shows the user's prompt, which must never reach a log.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The text the query matches.
    pub(crate) text: String,
    /// Bytes at the start of `text` drawn dimmed, such as `pane:`.
    pub(crate) dim: usize,
    /// The text on the right: a shortcut, or a state.
    pub(crate) hint: String,
    /// The agent state the row shows, if it stands for panes.
    pub(crate) state: Option<AgentState>,
    pub(crate) target: Target,
    /// How deep the row sits in the switcher's tree: 1 for a pane under
    /// its tab, 0 otherwise.
    pub(crate) depth: u8,
}

impl Entry {
    /// The text the row draws and how much of it is dimmed. In the
    /// switcher's tree a pane row sits under its tab, so the dimmed
    /// `project / tab ›` before its name is left out.
    pub(crate) fn drawn(&self, tree: bool) -> (&str, usize) {
        match self.text.get(self.dim..) {
            Some(rest) if tree && self.depth > 0 => (rest.strip_prefix(' ').unwrap_or(rest), 0),
            _ => (&self.text, self.dim),
        }
    }
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("text_chars", &self.text.chars().count())
            .field("hint", &self.hint)
            .field("state", &self.state)
            .field("target", &self.target)
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

/// A row that matches the query: an index and match positions, no text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// Index into the source list.
    pub(crate) entry: usize,
    /// Byte offsets of the matched characters in the entry's text.
    pub(crate) hits: Vec<usize>,
}

/// The open palette.
#[derive(Debug, Clone)]
pub(crate) struct Palette {
    mode: Mode,
    commands: Vec<Entry>,
    places: Vec<Entry>,
    query: String,
    rows: Vec<Row>,
    selected: usize,
    /// The row the switcher selects while the query is empty.
    home: Option<Target>,
    /// In [`Mode::Sessions`]: the project listed, the request its answer
    /// carries, whether it is still being read, and whether that failed.
    sessions_of: Option<ProjectId>,
    request: u64,
    loading: bool,
    failed: bool,
}

/// In the switcher, a query that starts with this lists commands instead,
/// as in the prototype.
const COMMAND_PREFIX: char = '>';

impl Palette {
    /// A palette with an empty query and the first row selected. `commands`
    /// and `places` are the rows of each mode.
    pub(crate) fn new(mode: Mode, commands: Vec<Entry>, places: Vec<Entry>) -> Self {
        let mut palette = Self {
            mode,
            commands,
            places,
            query: String::new(),
            rows: Vec::new(),
            selected: 0,
            home: None,
            sessions_of: None,
            request: 0,
            loading: false,
            failed: false,
        };
        palette.refilter();
        palette
    }

    /// The same palette, selecting the row that goes to `home` whenever the
    /// switcher's query is empty (RF-NAV-05).
    pub(crate) fn with_home(mut self, home: Option<Target>) -> Self {
        self.home = home;
        self.refilter();
        self
    }

    /// The past sessions list of `project`, empty while they are read; the
    /// core's answer to `request` fills it ([`Palette::set_sessions`]).
    pub(crate) fn sessions(project: ProjectId, request: u64) -> Self {
        let mut palette = Self::new(Mode::Sessions, Vec::new(), Vec::new());
        palette.sessions_of = Some(project);
        palette.request = request;
        palette.loading = true;
        palette
    }

    /// Fills the sessions list with `entries` when they answer the request
    /// it waits for; an answer for another project or an earlier request is
    /// ignored, so it cannot reset the list or the selection. The query is
    /// kept and the best match selected.
    pub(crate) fn set_sessions(
        &mut self,
        project: ProjectId,
        request: u64,
        entries: Vec<Entry>,
    ) -> bool {
        if self.mode != Mode::Sessions
            || self.sessions_of != Some(project)
            || self.request != request
            || !(self.loading || self.failed)
        {
            return false;
        }
        self.places = entries;
        self.loading = false;
        self.failed = false;
        self.refilter();
        true
    }

    /// Stops waiting for sessions after the core reported an error, so the
    /// list does not say it is reading forever; a late answer still fills it.
    pub(crate) fn stop_waiting(&mut self) -> bool {
        if !self.loading {
            return false;
        }
        self.loading = false;
        self.failed = true;
        true
    }

    /// The project whose sessions this lists, in [`Mode::Sessions`].
    pub(crate) fn sessions_of(&self) -> Option<ProjectId> {
        self.sessions_of
    }

    /// What the list says when no row shows; `project` names the project of
    /// a sessions list.
    pub(crate) fn empty_text(&self, project: &str) -> String {
        match self.listing() {
            Mode::Commands => format!("No command matches \"{}\".", self.search()),
            Mode::Switcher => format!("Nothing matches \"{}\".", self.query),
            Mode::Sessions if self.loading => "Reading past sessions…".to_owned(),
            Mode::Sessions if self.failed => {
                "Past sessions could not be read; the status bar says why.".to_owned()
            }
            Mode::Sessions if self.places.is_empty() => {
                format!("No past sessions for {project}.")
            }
            Mode::Sessions => format!("No session matches \"{}\".", self.query),
        }
    }

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    /// The list the query runs on now: the switcher lists commands while the
    /// query starts with `>`.
    pub(crate) fn listing(&self) -> Mode {
        match self.mode {
            Mode::Switcher if self.query.starts_with(COMMAND_PREFIX) => Mode::Commands,
            mode => mode,
        }
    }

    fn source(&self) -> &[Entry] {
        match self.listing() {
            Mode::Commands => &self.commands,
            Mode::Switcher | Mode::Sessions => &self.places,
        }
    }

    fn search(&self) -> &str {
        match (self.mode, self.listing()) {
            (Mode::Switcher, Mode::Commands) => &self.query[COMMAND_PREFIX.len_utf8()..],
            _ => &self.query,
        }
    }

    /// Changes the query; the best match becomes the selection.
    pub(crate) fn set_query(&mut self, query: &str) {
        if query != self.query {
            query.clone_into(&mut self.query);
            self.refilter();
        }
    }

    /// Whether the switcher lists every row as a tree: its query is empty,
    /// so the rows keep their order and panes sit under their tabs.
    pub(crate) fn shows_tree(&self) -> bool {
        self.listing() == Mode::Switcher && self.search().trim().is_empty()
    }

    fn refilter(&mut self) {
        let source = self.source();
        let rows: Vec<Row> = rank(self.search(), source.iter().map(|e| e.text.as_str()))
            .into_iter()
            .map(|(entry, Match { hits, .. })| Row { entry, hits })
            .collect();
        let home = match &self.home {
            Some(home) if self.shows_tree() => rows
                .iter()
                .position(|r| source.get(r.entry).is_some_and(|e| e.target == *home)),
            _ => None,
        };
        self.rows = rows;
        self.selected = home.unwrap_or(0);
    }

    /// The rows that match, best first.
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The entry a row shows.
    pub(crate) fn entry(&self, row: &Row) -> Option<&Entry> {
        self.source().get(row.entry)
    }

    /// The selected row's position, when any row matches.
    pub(crate) fn selected(&self) -> Option<usize> {
        (self.selected < self.rows.len()).then_some(self.selected)
    }

    /// Moves the selection by `delta` rows, stopping at either end.
    pub(crate) fn move_by(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// What running row `at` does; `None` when no row is there.
    pub(crate) fn target_at(&self, at: usize) -> Option<Target> {
        let row = self.rows.get(at)?;
        self.entry(row).map(|e| e.target.clone())
    }

    /// What running the selected row does.
    pub(crate) fn target(&self) -> Option<Target> {
        self.target_at(self.selected()?)
    }
}

/// The catalog as palette rows, each with the keys `shortcut` reads for it
/// from the live keymap, followed by a new tab and a split for each profile
/// (RF-NAV-02, RF-NAV-03, RF-AGE-04).
pub(crate) fn command_entries(
    shortcut: impl Fn(usize) -> Option<String>,
    profiles: &[ProfileInfo],
) -> Vec<Entry> {
    let mut entries: Vec<Entry> = CATALOG
        .iter()
        .enumerate()
        .map(|(at, spec)| Entry {
            text: spec.title.to_owned(),
            dim: spec.area_len(),
            hint: shortcut(at).unwrap_or_default(),
            state: None,
            target: Target::Command(at),
            depth: 0,
        })
        .collect();
    entries.extend(profile_tabs(profiles).into_iter().map(|tab| Entry {
        dim: "tab:".len(),
        text: tab.title,
        hint: String::new(),
        state: None,
        target: Target::NewTab(tab.profile),
        depth: 0,
    }));
    entries.extend(profile_splits(profiles).into_iter().map(|split| Entry {
        dim: "pane:".len(),
        text: split.title,
        hint: String::new(),
        state: None,
        target: Target::Split(split.profile, split.orientation),
        depth: 0,
    }));
    entries
}

/// The switcher's rows (RF-NAV-04) as a tree in the tab bar's order:
/// each tab as `project / tab`, then its panes in layout order one level
/// deeper; then the projects. A pane row names its project, tab and folder,
/// so two panes running the same program can be told apart when filtered;
/// a name two panes of one tab share gets the pane's place in it, `(2)`.
/// Each row shows its agent state, red when it needs you (RN-01).
pub(crate) fn switcher_entries(ws: &WorkspaceView) -> Vec<Entry> {
    let mut entries = Vec::new();
    for project in ws.projects() {
        for tab in ws.tabs_of(project) {
            let state = ws.tab_state(tab);
            let context = format!("{} / {}", project.name, tab.name);
            entries.push(Entry {
                dim: project.name.len() + " /".len(),
                text: context.clone(),
                hint: state.label().to_owned(),
                state: Some(state),
                target: Target::Tab(tab.id),
                depth: 0,
            });
            let panes: Vec<_> = tab
                .layout
                .panes()
                .into_iter()
                .map(|id| ws.pane(id))
                .collect();
            for (at, pane) in panes.iter().enumerate() {
                let id = pane.id;
                let mut text = format!("{context} › {}", pane.name);
                if panes.iter().filter(|p| p.name == pane.name).count() > 1 {
                    text.push_str(&format!(" ({})", at + 1));
                }
                let folder = folder_name(pane.cwd.as_deref().unwrap_or(&tab.dir));
                if !folder.is_empty() {
                    text.push_str(" · ");
                    text.push_str(folder);
                }
                let hint = if pane.profile.is_empty() {
                    pane.state.label().to_owned()
                } else {
                    format!("{} · {}", pane.profile, pane.state.label())
                };
                entries.push(Entry {
                    dim: context.len() + " ›".len(),
                    text,
                    hint,
                    state: Some(pane.state),
                    target: Target::Pane(id),
                    depth: 1,
                });
            }
        }
    }
    for project in ws.projects() {
        let tabs = ws.tabs_of(project).count();
        let waiting = ws.needs_you_tabs_of(project);
        let (hint, state) = if waiting > 0 {
            (format!("{waiting} need you"), Some(AgentState::NeedsYou))
        } else {
            let noun = if tabs == 1 { "tab" } else { "tabs" };
            (format!("project · {tabs} {noun}"), None)
        };
        entries.push(Entry {
            text: project.name.clone(),
            dim: 0,
            hint,
            state,
            target: Target::Project(project.id),
            depth: 0,
        });
    }
    entries
}

/// The row an empty switcher selects: the tab used before the current one,
/// so Ctrl+Shift+O then Enter goes back to it (RF-NAV-05).
pub(crate) fn switcher_home(ws: &WorkspaceView) -> Option<Target> {
    let selected = ws.selected_tab().map(|t| t.id);
    ws.tabs_by_recent()
        .into_iter()
        .find(|t| Some(t.id) != selected)
        .map(|t| Target::Tab(t.id))
}

/// The last component of a folder path, `/` or `\` separated.
fn folder_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rsplit(['/', '\\']).next() {
        Some(name) if !name.is_empty() => name,
        _ => path,
    }
}

/// The rows of a sessions list (RF-SES-07): each session's title, how long
/// ago it changed and the start of its id. A session a pane already runs is
/// marked open, and its row goes to that pane.
pub(crate) fn session_entries(
    project: ProjectId,
    sessions: &[AgentSession],
    ws: &WorkspaceView,
    now_ms: u64,
) -> Vec<Entry> {
    sessions
        .iter()
        .map(|s| {
            let text = s
                .title
                .as_ref()
                .map_or_else(|| NO_TITLE.to_owned(), |t| t.as_str().to_owned());
            let id: String = s.id.chars().take(ID_PREFIX).collect();
            let when = relative_time(s.modified_ms, now_ms);
            let (hint, state, target) = match ws.pane_with_session(&s.id) {
                Some(pane) => (
                    format!("{when} · {id} · open"),
                    Some(ws.pane(pane).state),
                    Target::Pane(pane),
                ),
                None => (
                    format!("{when} · {id}"),
                    None,
                    Target::Resume {
                        project,
                        profile: s.profile.clone(),
                        session_id: s.id.clone(),
                    },
                ),
            };
            Entry {
                text,
                dim: 0,
                hint,
                state,
                target,
                depth: 0,
            }
        })
        .collect()
}

/// A session row with no prompt found shows this.
const NO_TITLE: &str = "(no prompt)";

/// The characters of a session id a row shows.
const ID_PREFIX: usize = 8;

/// How long before `now_ms` the time `then_ms` was, in the largest unit
/// that fits: "just now", "5 min ago", "3 h ago", "2 d ago", "4 mo ago". A
/// time in the future (a clock moved back) is "just now".
pub(crate) fn relative_time(then_ms: u64, now_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    let (mins, hours, days) = (secs / 60, secs / 3600, secs / 86_400);
    if mins == 0 {
        "just now".to_owned()
    } else if hours == 0 {
        format!("{mins} min ago")
    } else if days == 0 {
        format!("{hours} h ago")
    } else if days < 30 {
        format!("{days} d ago")
    } else if days < 365 {
        format!("{} mo ago", days / 30)
    } else {
        format!("{} y ago", days / 365)
    }
}

/// The pane that has the keyboard whenever the palette is closed: the
/// selected tab's focused pane, as the core last reported it. Focus moves
/// made while the palette was open (Alt+arrows) therefore hold on close.
pub(crate) fn keyboard_pane(ws: &WorkspaceView) -> Option<PaneId> {
    ws.selected_tab().map(|t| t.focused)
}

/// Where the keyboard goes when the palette closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyboardTo {
    /// The pane the core says is focused.
    Pane(PaneId),
    /// The window itself, when no tab is open, so its keys still work.
    Window,
}

/// The palette's close step decides this; the window then focuses it
/// unconditionally, since the palette's input held the focus until now.
pub(crate) fn keyboard_after_close(ws: &WorkspaceView) -> KeyboardTo {
    keyboard_pane(ws).map_or(KeyboardTo::Window, KeyboardTo::Pane)
}

/// How each stretch of a row's text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ink {
    /// Plain text.
    Text,
    /// The dimmed prefix.
    Dim,
    /// A character the query matched.
    Hit,
}

/// Splits `text` into runs of one [`Ink`]: matched characters over the
/// dimmed prefix over plain text.
pub(crate) fn ink_runs(text: &str, dim: usize, hits: &[usize]) -> Vec<(Range<usize>, Ink)> {
    let mut runs: Vec<(Range<usize>, Ink)> = Vec::new();
    for (at, c) in text.char_indices() {
        let ink = if hits.contains(&at) {
            Ink::Hit
        } else if at < dim {
            Ink::Dim
        } else {
            Ink::Text
        };
        let end = at + c.len_utf8();
        match runs.last_mut() {
            Some((range, last)) if *last == ink => range.end = end,
            _ => runs.push((at..end, ink)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use yien_proto::{
        AgentState as P, LayoutChild, LayoutNode, NeedsReason, Orientation, PaneInfo, ProjectInfo,
        TabInfo, WorkspaceSnapshot,
    };

    use super::*;

    fn tab(id: u64, project: u64, name: &str, panes: &[u64]) -> TabInfo {
        let layout = match panes {
            [one] => LayoutNode::Pane(PaneId::new(*one)),
            many => LayoutNode::Split {
                orientation: Orientation::Horizontal,
                children: many
                    .iter()
                    .map(|p| LayoutChild {
                        flex: 1.0,
                        node: LayoutNode::Pane(PaneId::new(*p)),
                    })
                    .collect(),
            },
        };
        TabInfo {
            id: TabId::new(id),
            project: ProjectId::new(project),
            name: name.into(),
            dir: PROJECT_DIRS[usize::try_from(project).unwrap() - 1].into(),
            branch: None,
            layout,
            focused: PaneId::new(panes[0]),
            maximized: None,
            muted: false,
            worktree: None,
        }
    }

    fn pane(id: u64, tab: u64, name: &str, state: P) -> PaneInfo {
        PaneInfo {
            id: PaneId::new(id),
            tab: TabId::new(tab),
            name: name.into(),
            profile: "Shell".into(),
            profile_id: yien_proto::ProfileId::new("shell"),
            title: None,
            cwd: None,
            state,
            running: true,
            exit_code: None,
            session_id: None,
            needs_you_since: state.is_red().then_some(id),
            attention_since: state.is_red().then_some(id),
            question: None,
            signal_lost: false,
            runs_session: true,
        }
    }

    const NEEDS: P = P::NeedsYou(NeedsReason::Permission);

    /// The folder the tabs of project 1, 2 and 3 start in.
    const PROJECT_DIRS: [&str; 3] = ["C:/Code/yien", "D:\\notes\\", "C:/Code/app"];

    fn project(id: u64, name: &str, tabs: &[u64]) -> ProjectInfo {
        ProjectInfo {
            id: ProjectId::new(id),
            name: name.into(),
            root: "C:/Code".into(),
            tabs: tabs.iter().map(|t| TabId::new(*t)).collect(),
            collapsed: false,
        }
    }

    fn view(projects: Vec<ProjectInfo>, tabs: Vec<TabInfo>, panes: Vec<PaneInfo>) -> WorkspaceView {
        let active_tab = tabs.first().map(|t| t.id);
        WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects,
            tabs,
            panes,
            active_tab,
            profiles: vec![],
            notifications_muted: false,
            side_bars: Default::default(),
            terminal_zoom: 0,
        })
    }

    /// yien: "editor" (panes 100 pwsh, 101 claude needs you), "server" (110
    /// cargo, in `crates/yien-ui`); notes: "draft" (200). "editor" is
    /// selected.
    fn workspace() -> WorkspaceView {
        let mut cargo = pane(110, 11, "cargo", P::Working);
        cargo.cwd = Some("C:/Code/yien/crates/yien-ui".into());
        view(
            vec![project(1, "yien", &[10, 11]), project(2, "notes", &[20])],
            vec![
                tab(10, 1, "editor", &[100, 101]),
                tab(11, 1, "server", &[110]),
                tab(20, 2, "draft", &[200]),
            ],
            vec![
                pane(100, 10, "pwsh", P::Idle),
                pane(101, 10, "claude", NEEDS),
                cargo,
                pane(200, 20, "pwsh", P::Idle),
            ],
        )
    }

    fn switcher(ws: &WorkspaceView) -> Palette {
        Palette::new(Mode::Switcher, commands(), switcher_entries(ws)).with_home(switcher_home(ws))
    }

    fn commands() -> Vec<Entry> {
        command_entries(|_| None, &[])
    }

    fn texts(palette: &Palette) -> Vec<String> {
        palette
            .rows()
            .iter()
            .filter_map(|r| palette.entry(r))
            .map(|e| e.text.clone())
            .collect()
    }

    fn command_index(id: &str) -> usize {
        CATALOG.iter().position(|s| s.id == id).unwrap()
    }

    #[test]
    fn rf_nav_02_the_palette_lists_the_whole_catalog() {
        let palette = Palette::new(Mode::Commands, commands(), Vec::new());
        assert_eq!(palette.rows().len(), CATALOG.len());
        assert_eq!(palette.selected(), Some(0));
        assert_eq!(palette.target(), Some(Target::Command(0)));
    }

    #[test]
    fn rf_nav_01_typing_filters_and_the_best_match_runs() {
        let mut palette = Palette::new(Mode::Commands, commands(), Vec::new());
        palette.set_query("split");
        assert!(texts(&palette).iter().all(|t| t.contains("split")));
        assert_eq!(texts(&palette)[0], "pane: split right");
        assert_eq!(
            palette.target(),
            Some(Target::Command(command_index("pane.split_right")))
        );
        palette.set_query("zzz");
        assert!(palette.rows().is_empty());
        assert_eq!(palette.selected(), None);
        assert_eq!(palette.target(), None);
    }

    #[test]
    fn rf_nav_01_up_and_down_move_the_selection_within_the_rows() {
        let mut palette = Palette::new(Mode::Commands, commands(), Vec::new());
        palette.set_query("split");
        let count = palette.rows().len();
        assert!(count >= 3);
        palette.move_by(-1);
        assert_eq!(palette.selected(), Some(0));
        palette.move_by(1);
        palette.move_by(1);
        assert_eq!(palette.selected(), Some(2));
        assert_eq!(palette.target(), palette.target_at(2));
        palette.move_by(100);
        assert_eq!(palette.selected(), Some(count - 1));
        palette.set_query("spli");
        assert_eq!(
            palette.selected(),
            Some(0),
            "a new query selects the best match"
        );
    }

    #[test]
    fn rf_nav_03_each_command_row_carries_its_shortcut() {
        let split = command_index("pane.split_right");
        let entries = command_entries(|at| (at == split).then(|| "Alt+Shift+=".to_owned()), &[]);
        assert_eq!(entries[split].hint, "Alt+Shift+=");
        assert_eq!(entries[split].dim, "pane:".len());
        assert!(entries.iter().filter(|e| e.hint.is_empty()).count() == CATALOG.len() - 1);
    }

    #[test]
    fn rf_nav_04_the_switcher_lists_each_tab_with_its_panes_then_the_projects() {
        assert_eq!(
            texts(&switcher(&workspace())),
            [
                "yien / editor",
                "yien / editor › pwsh · yien",
                "yien / editor › claude · yien",
                "yien / server",
                "yien / server › cargo · yien-ui",
                "notes / draft",
                "notes / draft › pwsh · notes",
                "yien",
                "notes",
            ]
        );
    }

    #[test]
    fn rf_nav_04_unrenamed_tabs_of_two_projects_are_told_apart() {
        let mut moved = pane(200, 20, "powershell", P::Idle);
        moved.cwd = Some("C:\\Users\\me\\scratch".into());
        let ws = view(
            vec![project(1, "yien", &[10]), project(2, "notes", &[20])],
            vec![tab(10, 1, "Tab 1", &[100]), tab(20, 2, "Tab 1", &[200])],
            vec![pane(100, 10, "powershell", P::Idle), moved],
        );
        let mut palette = switcher(&ws);
        let drawn = |palette: &Palette| -> Vec<String> {
            let tree = palette.shows_tree();
            palette
                .rows()
                .iter()
                .filter_map(|r| palette.entry(r))
                .map(|e| e.drawn(tree).0.to_owned())
                .collect()
        };
        assert!(palette.shows_tree());
        assert_eq!(
            drawn(&palette),
            [
                "yien / Tab 1",
                "powershell · yien",
                "notes / Tab 1",
                "powershell · scratch",
                "yien",
                "notes",
            ]
        );
        palette.set_query("powershell");
        assert!(!palette.shows_tree());
        assert_eq!(
            drawn(&palette),
            [
                "yien / Tab 1 › powershell · yien",
                "notes / Tab 1 › powershell · scratch",
            ]
        );
    }

    #[test]
    fn rf_nav_04_a_tabs_panes_follow_it_in_layout_order_one_level_deeper() {
        let mut build = tab(30, 3, "build", &[302]);
        build.layout = LayoutNode::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                LayoutChild {
                    flex: 1.0,
                    node: LayoutNode::Pane(PaneId::new(302)),
                },
                LayoutChild {
                    flex: 1.0,
                    node: tab(0, 3, "", &[300, 301]).layout,
                },
            ],
        };
        let ws = view(
            vec![project(3, "app", &[30, 31])],
            vec![build, tab(31, 3, "docs", &[310])],
            vec![
                pane(300, 30, "claude", P::Working),
                pane(301, 30, "codex", P::Idle),
                pane(302, 30, "pwsh", P::Idle),
                pane(310, 31, "pwsh", P::Idle),
            ],
        );
        let entries = switcher_entries(&ws);
        let tree: Vec<(Target, u8)> = entries
            .iter()
            .map(|e| (e.target.clone(), e.depth))
            .collect();
        assert_eq!(
            tree,
            [
                (Target::Tab(TabId::new(30)), 0),
                (Target::Pane(PaneId::new(302)), 1),
                (Target::Pane(PaneId::new(300)), 1),
                (Target::Pane(PaneId::new(301)), 1),
                (Target::Tab(TabId::new(31)), 0),
                (Target::Pane(PaneId::new(310)), 1),
                (Target::Project(ProjectId::new(3)), 0),
            ]
        );
        assert_eq!(entries[1].drawn(true), ("pwsh · app", 0));
        assert_eq!(
            entries[1].drawn(false),
            ("app / build › pwsh · app", "app / build ›".len())
        );
        assert_eq!(entries[0].drawn(true), ("app / build", "app /".len()));
        assert_eq!(entries[1].hint, "Shell · Idle");
    }

    #[test]
    fn rf_nav_04_panes_of_one_tab_with_the_same_name_carry_their_place() {
        let ws = view(
            vec![project(1, "yien", &[10])],
            vec![tab(10, 1, "Tab 1", &[100, 101, 102])],
            vec![
                pane(100, 10, "powershell", P::Idle),
                pane(101, 10, "claude", P::Idle),
                pane(102, 10, "powershell", P::Idle),
            ],
        );
        let mut palette = switcher(&ws);
        let tree = palette.shows_tree();
        let drawn: Vec<String> = palette
            .rows()
            .iter()
            .filter_map(|r| palette.entry(r))
            .map(|e| e.drawn(tree).0.to_owned())
            .collect();
        assert_eq!(
            drawn,
            [
                "yien / Tab 1",
                "powershell (1) · yien",
                "claude · yien",
                "powershell (3) · yien",
                "yien",
            ]
        );
        palette.set_query("powershell");
        assert_eq!(
            texts(&palette),
            [
                "yien / Tab 1 › powershell (1) · yien",
                "yien / Tab 1 › powershell (3) · yien",
            ]
        );
    }

    #[test]
    fn rf_nav_04_a_query_keeps_each_matching_panes_context() {
        let mut palette = switcher(&workspace());
        palette.set_query("pwsh");
        assert_eq!(
            texts(&palette),
            [
                "yien / editor › pwsh · yien",
                "notes / draft › pwsh · notes"
            ]
        );
        let first = palette.entry(&palette.rows()[0]).unwrap();
        assert_eq!(first.drawn(palette.shows_tree()).1, "yien / editor ›".len());
        // A tab's name lists the tab, then its panes.
        palette.set_query("server");
        assert_eq!(
            texts(&palette),
            ["yien / server", "yien / server › cargo · yien-ui"]
        );
        // The pane's folder matches too.
        palette.set_query("yien-ui");
        assert_eq!(palette.target(), Some(Target::Pane(PaneId::new(110))));
    }

    #[test]
    fn rf_nav_04_rows_that_need_you_are_marked() {
        let entries = switcher_entries(&workspace());
        let state = |text: &str| entries.iter().find(|e| e.text == text).unwrap().state;
        assert_eq!(state("yien / editor"), Some(AgentState::NeedsYou));
        assert_eq!(
            state("yien / editor › claude · yien"),
            Some(AgentState::NeedsYou)
        );
        assert_eq!(state("yien"), Some(AgentState::NeedsYou));
        assert_eq!(state("yien / server"), Some(AgentState::Working));
        assert_eq!(state("yien / editor › pwsh · yien"), Some(AgentState::Idle));
        assert_eq!(state("notes"), None);
        let notes = entries.iter().find(|e| e.text == "notes").unwrap();
        assert_eq!(notes.hint, "project · 1 tab");
    }

    #[test]
    fn rf_nav_04_typing_a_tab_name_selects_that_tab() {
        let mut palette = switcher(&workspace());
        palette.set_query("draf");
        assert_eq!(palette.target(), Some(Target::Tab(TabId::new(20))));
        palette.set_query("claude");
        assert_eq!(palette.target(), Some(Target::Pane(PaneId::new(101))));
        palette.set_query("notes");
        assert_eq!(palette.target(), Some(Target::Tab(TabId::new(20))));
    }

    #[test]
    fn rf_nav_05_enter_on_an_empty_switcher_goes_to_the_previous_tab() {
        let mut ws = workspace();
        ws.select_tab(TabId::new(20));
        ws.select_tab(TabId::new(10));
        let mut palette = switcher(&ws);
        assert_eq!(palette.target(), Some(Target::Tab(TabId::new(20))));
        assert_eq!(palette.selected(), Some(5));
        palette.set_query("claude");
        assert_eq!(palette.selected(), Some(0));
        palette.set_query("");
        assert_eq!(palette.target(), Some(Target::Tab(TabId::new(20))));
        palette.move_by(-1);
        assert_eq!(palette.target(), Some(Target::Pane(PaneId::new(110))));
    }

    #[test]
    fn rf_nav_05_an_only_tab_is_the_first_row() {
        let ws = view(
            vec![project(1, "yien", &[10])],
            vec![tab(10, 1, "editor", &[100])],
            vec![pane(100, 10, "pwsh", P::Idle)],
        );
        assert_eq!(switcher_home(&ws), None);
        assert_eq!(switcher(&ws).target(), Some(Target::Tab(TabId::new(10))));
    }

    #[test]
    fn rf_nav_04_a_pane_row_shows_the_last_folder_of_its_path() {
        assert_eq!(folder_name("C:/Code/yien"), "yien");
        assert_eq!(folder_name("C:\\Code\\yien\\"), "yien");
        assert_eq!(folder_name("C:\\"), "C:");
        assert_eq!(folder_name("/"), "/");
        assert_eq!(folder_name("~"), "~");
        assert_eq!(folder_name(""), "");
    }

    #[test]
    fn rf_nav_04_a_leading_angle_bracket_lists_commands() {
        let mut palette = Palette::new(Mode::Switcher, commands(), switcher_entries(&workspace()));
        palette.set_query(">split");
        assert_eq!(palette.listing(), Mode::Commands);
        assert_eq!(texts(&palette)[0], "pane: split right");
        palette.set_query("split");
        assert_eq!(palette.listing(), Mode::Switcher);
        assert!(palette.rows().is_empty());
    }

    #[test]
    fn rf_nav_01_closing_follows_a_focus_move_made_while_open() {
        let mut ws = workspace();
        assert_eq!(
            keyboard_after_close(&ws),
            KeyboardTo::Pane(PaneId::new(100))
        );
        let palette = Palette::new(Mode::Commands, commands(), Vec::new());
        // Alt+Right reaches the core while the palette is open.
        let mut moved = tab(10, 1, "editor", &[100, 101]);
        moved.focused = PaneId::new(101);
        ws.tab_updated(&moved);
        drop(palette);
        assert_eq!(
            keyboard_after_close(&ws),
            KeyboardTo::Pane(PaneId::new(101))
        );
    }

    #[test]
    fn rf_nav_01_closing_with_no_tab_gives_the_keyboard_to_the_window() {
        let mut ws = workspace();
        for id in [10, 11, 20] {
            ws.tab_closed(TabId::new(id));
        }
        assert_eq!(keyboard_pane(&ws), None);
        assert_eq!(keyboard_after_close(&ws), KeyboardTo::Window);
        assert_eq!(
            keyboard_after_close(&WorkspaceView::default()),
            KeyboardTo::Window
        );
    }

    #[test]
    fn rf_nav_02_every_profile_split_is_a_command_row() {
        let profiles = [yien_proto::ProfileInfo {
            id: ProfileId::new("claude"),
            name: "Claude Code".into(),
        }];
        let entries = command_entries(|_| None, &profiles);
        assert_eq!(entries.len(), CATALOG.len() + 1);
        let mut palette = Palette::new(Mode::Commands, entries, Vec::new());
        palette.set_query("split down with claude");
        assert_eq!(
            palette.target(),
            Some(Target::Split(
                ProfileId::new("claude"),
                Orientation::Vertical
            ))
        );
    }

    #[test]
    fn rf_age_04_the_palette_lists_a_profile_added_by_the_user() {
        let info = |id: &str, name: &str| yien_proto::ProfileInfo {
            id: ProfileId::new(id),
            name: name.into(),
        };
        let profiles = [info("shell", "Shell"), info("mytool", "My tool")];
        let entries = command_entries(|_| None, &profiles);
        let mut palette = Palette::new(Mode::Commands, entries, Vec::new());
        palette.set_query("new tab with my tool");
        assert_eq!(
            palette.target(),
            Some(Target::NewTab(ProfileId::new("mytool")))
        );
        palette.set_query("split right with my tool");
        assert_eq!(
            palette.target(),
            Some(Target::Split(
                ProfileId::new("mytool"),
                Orientation::Horizontal
            ))
        );
    }

    const NOW: u64 = 1_762_500_000_000;
    const HOUR: u64 = 3_600_000;

    fn past(id: &str, title: Option<&str>, ago: u64) -> AgentSession {
        AgentSession {
            profile: ProfileId::new("claude"),
            id: id.into(),
            title: title.map(yien_proto::SessionTitle::new),
            modified_ms: NOW - ago,
        }
    }

    fn listed(ws: &WorkspaceView) -> Palette {
        let sessions = [
            past(
                "aaaaaaaa-0000-4000-8000-000000000001",
                Some("fix the build"),
                HOUR,
            ),
            past(
                "bbbbbbbb-0000-4000-8000-000000000002",
                Some("write the docs"),
                3 * HOUR,
            ),
            past("cccccccc-0000-4000-8000-000000000003", None, 50 * HOUR),
        ];
        let mut palette = Palette::sessions(ProjectId::new(1), 1);
        let entries = session_entries(ProjectId::new(1), &sessions, ws, NOW);
        assert!(palette.set_sessions(ProjectId::new(1), 1, entries));
        palette
    }

    #[test]
    fn rf_ses_07_the_list_shows_title_age_and_id_start() {
        let palette = listed(&workspace());
        assert_eq!(
            texts(&palette),
            ["fix the build", "write the docs", "(no prompt)"]
        );
        let hints: Vec<String> = palette
            .rows()
            .iter()
            .filter_map(|r| palette.entry(r))
            .map(|e| e.hint.clone())
            .collect();
        assert_eq!(
            hints,
            [
                "1 h ago · aaaaaaaa",
                "3 h ago · bbbbbbbb",
                "2 d ago · cccccccc"
            ]
        );
        assert_eq!(
            palette.target(),
            Some(Target::Resume {
                project: ProjectId::new(1),
                profile: ProfileId::new("claude"),
                session_id: "aaaaaaaa-0000-4000-8000-000000000001".into(),
            })
        );
    }

    #[test]
    fn rf_ses_07_typing_filters_the_sessions() {
        let mut palette = listed(&workspace());
        palette.set_query("docs");
        assert_eq!(texts(&palette), ["write the docs"]);
        assert!(matches!(
            palette.target(),
            Some(Target::Resume { session_id, .. }) if session_id.starts_with("bbbbbbbb")
        ));
        palette.set_query("zzz");
        assert_eq!(palette.target(), None);
        assert_eq!(palette.empty_text("yien"), "No session matches \"zzz\".");
    }

    #[test]
    fn rf_ses_07_the_list_says_when_it_reads_and_when_there_is_none() {
        let mut palette = Palette::sessions(ProjectId::new(1), 5);
        assert!(palette.rows().is_empty());
        assert_eq!(palette.empty_text("yien"), "Reading past sessions…");
        // An answer for another project or request leaves it waiting.
        assert!(!palette.set_sessions(ProjectId::new(2), 5, Vec::new()));
        assert!(!palette.set_sessions(ProjectId::new(1), 4, Vec::new()));
        assert_eq!(palette.empty_text("yien"), "Reading past sessions…");
        assert!(palette.set_sessions(ProjectId::new(1), 5, Vec::new()));
        assert_eq!(palette.empty_text("yien"), "No past sessions for yien.");
        assert_eq!(palette.sessions_of(), Some(ProjectId::new(1)));
        // Only a sessions list takes sessions.
        let mut commands = Palette::new(Mode::Commands, commands(), Vec::new());
        assert!(!commands.set_sessions(ProjectId::new(1), 0, Vec::new()));
        assert!(!commands.stop_waiting());
    }

    #[test]
    fn rf_ses_07_an_error_stops_the_waiting_and_a_late_answer_still_fills() {
        let mut palette = Palette::sessions(ProjectId::new(1), 2);
        assert!(palette.stop_waiting());
        assert_eq!(
            palette.empty_text("yien"),
            "Past sessions could not be read; the status bar says why."
        );
        assert!(!palette.stop_waiting());
        let sessions = [past(
            "aaaaaaaa-0000-4000-8000-000000000001",
            Some("fix the build"),
            HOUR,
        )];
        let entries = session_entries(ProjectId::new(1), &sessions, &workspace(), NOW);
        assert!(palette.set_sessions(ProjectId::new(1), 2, entries));
        assert_eq!(texts(&palette), ["fix the build"]);
    }

    #[test]
    fn rf_ses_07_a_second_answer_does_not_reset_the_list() {
        let mut palette = listed(&workspace());
        palette.move_by(1);
        let selected = palette.target();
        assert!(!palette.set_sessions(ProjectId::new(1), 1, Vec::new()));
        assert_eq!(palette.rows().len(), 3);
        assert_eq!(palette.target(), selected);
    }

    #[test]
    fn rf_ses_07_a_query_typed_while_reading_applies_to_the_answer() {
        let mut palette = Palette::sessions(ProjectId::new(1), 1);
        palette.set_query("docs");
        let sessions = [
            past(
                "aaaaaaaa-0000-4000-8000-000000000001",
                Some("fix the build"),
                HOUR,
            ),
            past(
                "bbbbbbbb-0000-4000-8000-000000000002",
                Some("write the docs"),
                HOUR,
            ),
        ];
        let entries = session_entries(ProjectId::new(1), &sessions, &workspace(), NOW);
        palette.set_sessions(ProjectId::new(1), 1, entries);
        assert_eq!(texts(&palette), ["write the docs"]);
    }

    #[test]
    fn rf_ses_07_a_session_whose_pane_stopped_resumes_again() {
        for stopped in ["exited", "stand-in"] {
            let mut ws = workspace();
            let mut pane = pane(101, 10, "claude", P::Idle);
            pane.session_id = Some("bbbbbbbb-0000-4000-8000-000000000002".into());
            pane.runs_session = false;
            if stopped == "exited" {
                pane.running = false;
                pane.exit_code = Some(0);
            }
            ws.pane_updated(&pane);
            let mut palette = listed(&ws);
            palette.set_query("docs");
            let entry = palette.entry(&palette.rows()[0]).unwrap();
            assert_eq!(entry.hint, "3 h ago · bbbbbbbb", "{stopped}");
            assert_eq!(entry.state, None, "{stopped}");
            assert!(
                matches!(palette.target(), Some(Target::Resume { .. })),
                "{stopped}"
            );
        }
        // An exit reported as a delta stops it too.
        let mut ws = workspace();
        let mut pane = pane(101, 10, "claude", P::Idle);
        pane.session_id = Some("bbbbbbbb-0000-4000-8000-000000000002".into());
        ws.pane_updated(&pane);
        assert_eq!(
            ws.pane_with_session("bbbbbbbb-0000-4000-8000-000000000002"),
            Some(PaneId::new(101))
        );
        ws.pane_exited(PaneId::new(101), Some(0));
        assert_eq!(
            ws.pane_with_session("bbbbbbbb-0000-4000-8000-000000000002"),
            None
        );
    }

    #[test]
    fn rf_ses_07_a_session_already_open_is_marked_and_goes_to_its_pane() {
        let mut ws = workspace();
        let mut running = pane(101, 10, "claude", NEEDS);
        running.session_id = Some("BBBBBBBB-0000-4000-8000-000000000002".into());
        ws.pane_updated(&running);
        let mut palette = listed(&ws);
        palette.set_query("docs");
        let entry = palette.entry(&palette.rows()[0]).unwrap();
        assert_eq!(entry.hint, "3 h ago · bbbbbbbb · open");
        assert_eq!(entry.state, Some(AgentState::NeedsYou));
        assert_eq!(palette.target(), Some(Target::Pane(PaneId::new(101))));
    }

    #[test]
    fn rf_ses_07_a_row_never_shows_its_text_in_debug() {
        let palette = listed(&workspace());
        let shown = format!("{palette:?}");
        assert!(!shown.contains("fix the build"), "{shown}");
        assert!(shown.contains("text_chars: 13"), "{shown}");
    }

    #[test]
    fn rf_ses_07_ages_read_in_the_largest_unit_that_fits() {
        let at = |ago_ms: u64| relative_time(NOW - ago_ms, NOW);
        assert_eq!(at(0), "just now");
        assert_eq!(at(59_000), "just now");
        assert_eq!(at(60_000), "1 min ago");
        assert_eq!(at(HOUR - 1), "59 min ago");
        assert_eq!(at(HOUR), "1 h ago");
        assert_eq!(at(24 * HOUR - 1), "23 h ago");
        assert_eq!(at(24 * HOUR), "1 d ago");
        assert_eq!(at(29 * 24 * HOUR), "29 d ago");
        assert_eq!(at(45 * 24 * HOUR), "1 mo ago");
        assert_eq!(at(800 * 24 * HOUR), "2 y ago");
        assert_eq!(relative_time(NOW + HOUR, NOW), "just now");
    }

    #[test]
    fn hits_are_drawn_over_the_dimmed_prefix() {
        assert_eq!(
            ink_runs("pane: split", 5, &[0, 6, 7]),
            vec![
                (0..1, Ink::Hit),
                (1..5, Ink::Dim),
                (5..6, Ink::Text),
                (6..8, Ink::Hit),
                (8..11, Ink::Text),
            ]
        );
        assert_eq!(
            ink_runs("año", 0, &[1]),
            vec![(0..1, Ink::Text), (1..3, Ink::Hit), (3..4, Ink::Text)]
        );
    }
}
