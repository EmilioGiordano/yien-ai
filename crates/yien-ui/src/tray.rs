//! The attention tray (RF-ATN-13) and the quit question (RF-SES-13), as
//! plain functions of the view model; `shell` draws them.

use yien_proto::{NeedsReason, PaneId, TabId};

use crate::model::{AgentState, WorkspaceView};

/// One pane in the attention tray. Its `Debug` leaves out the detail, which
/// may be the agent's question (plan §5.4).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TrayRow {
    pub(crate) pane: PaneId,
    pub(crate) tab: TabId,
    pub(crate) state: AgentState,
    /// `project / tab › pane`.
    pub(crate) place: String,
    /// The profile it runs: `Claude Code`.
    pub(crate) profile: String,
    /// What it asks, or what happened.
    pub(crate) detail: String,
}

impl std::fmt::Debug for TrayRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrayRow")
            .field("pane", &self.pane)
            .field("tab", &self.tab)
            .field("state", &self.state)
            .field("detail_chars", &self.detail.chars().count())
            .finish_non_exhaustive()
    }
}

/// The states the tray lists, in the order it lists them.
const TRAY_STATES: [AgentState; 3] = [AgentState::NeedsYou, AgentState::Error, AgentState::Done];

/// Every pane that needs the user, failed or finished, across all tabs and
/// projects: needs you first, then error, then done, the one there longest
/// first within each (RF-ATN-13).
pub(crate) fn tray_rows(workspace: &WorkspaceView) -> Vec<TrayRow> {
    TRAY_STATES
        .into_iter()
        .flat_map(|state| workspace.panes_in(state))
        .filter_map(|id| {
            let tab = workspace.tab_of(id)?;
            let project = workspace
                .project(tab.project)
                .map(|p| p.name.as_str())
                .unwrap_or_default();
            let pane = workspace.pane(id);
            Some(TrayRow {
                pane: id,
                tab: tab.id,
                state: pane.state,
                place: format!("{project} / {} › {}", tab.name, pane.name),
                profile: pane.profile.clone(),
                detail: detail(&pane),
            })
        })
        .collect()
}

/// What a row says under its place: the agent's own question when its
/// hooks gave one, else why it waits; for an error the exit code when
/// there is one.
fn detail(pane: &crate::model::PaneView) -> String {
    match pane.state {
        AgentState::NeedsYou => match (&pane.question, pane.needs_reason) {
            (Some(question), _) => question.as_str().to_owned(),
            (None, Some(NeedsReason::Permission)) => "Asks for permission".to_owned(),
            (None, Some(NeedsReason::Question)) => "Has a question".to_owned(),
            (None, Some(NeedsReason::Blocked)) => "Is blocked".to_owned(),
            (None, Some(NeedsReason::ScreenRule) | None) => "Is waiting for you".to_owned(),
        },
        AgentState::Error => match pane.exit_code {
            Some(code) if pane.exited => format!("Stopped with exit code {code}"),
            _ => "The turn failed".to_owned(),
        },
        _ => "Turn finished".to_owned(),
    }
}

/// The tray's cursor after the rows changed: the same pane while it is
/// still listed, else the row now at its old place, else the last row.
pub(crate) fn keep_cursor(
    rows: &[TrayRow],
    cursor: Option<PaneId>,
    old_index: usize,
) -> Option<PaneId> {
    let cursor = cursor.filter(|c| rows.iter().any(|r| r.pane == *c));
    cursor.or_else(|| {
        rows.get(old_index.min(rows.len().saturating_sub(1)))
            .map(|r| r.pane)
    })
}

/// Where `pane`'s row sits among the tray list's children, which put a
/// head before each state's rows.
pub(crate) fn list_index(rows: &[TrayRow], pane: PaneId) -> Option<usize> {
    let at = rows.iter().position(|r| r.pane == pane)?;
    let heads = 1 + rows[..=at]
        .windows(2)
        .filter(|w| w[0].state != w[1].state)
        .count();
    Some(at + heads)
}

/// The tray's cursor moved by `by` rows, wrapping around.
pub(crate) fn move_cursor(rows: &[TrayRow], cursor: Option<PaneId>, by: isize) -> Option<PaneId> {
    if rows.is_empty() {
        return None;
    }
    let len = rows.len() as isize;
    let at = cursor
        .and_then(|c| rows.iter().position(|r| r.pane == c))
        .map_or(if by > 0 { -1 } else { 0 }, |at| at as isize);
    let next = (at + by).rem_euclid(len);
    rows.get(next as usize).map(|r| r.pane)
}

/// What a request to close the window does (RF-SES-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseRequest {
    /// Nothing is working, or the user already said to quit: close.
    Close,
    /// Agents are working: ask, naming how many.
    Ask(usize),
    /// The question is already up: leave it, and keep the window.
    StillAsking,
}

/// Whether closing the window should ask first: only while an agent is
/// working, and never a second time once the user chose to quit.
pub(crate) fn close_request(working: usize, asking: bool, confirmed: bool) -> CloseRequest {
    if confirmed || working == 0 {
        CloseRequest::Close
    } else if asking {
        CloseRequest::StillAsking
    } else {
        CloseRequest::Ask(working)
    }
}

/// The quit key pressed (RF-SES-13). While the quit question is open it
/// answers yes only when `armed`: a new press, not the press that opened
/// the question repeating while held.
pub(crate) fn quit_key_request(
    working: usize,
    asking: bool,
    armed: bool,
    confirmed: bool,
) -> CloseRequest {
    match (asking, armed) {
        (true, true) => CloseRequest::Close,
        (true, false) => CloseRequest::StillAsking,
        (false, _) => close_request(working, false, confirmed),
    }
}

/// Whether a key coming up arms the quit key: only the quit key's own key.
/// Another key released meanwhile (rollover) leaves a held quit key held.
pub(crate) fn releases_quit_key(released: &str, quit_key: Option<&str>) -> bool {
    quit_key.is_some_and(|key| key.eq_ignore_ascii_case(released))
}

/// What the quit question says about the working agents.
pub(crate) fn quit_detail(working: usize) -> String {
    match working {
        0 => "No agent is working any more.".to_owned(),
        1 => "1 agent is still working. Quitting stops it mid-turn; its session comes back \
              when you open YIEN again."
            .to_owned(),
        n => format!(
            "{n} agents are still working. Quitting stops them mid-turn; their sessions come \
             back when you open YIEN again."
        ),
    }
}

#[cfg(test)]
mod tests {
    use yien_proto::{
        AgentState as P, ErrorReason, LayoutChild, LayoutNode, Orientation, PaneInfo,
        PendingQuestion, ProfileId, ProjectId, ProjectInfo, TabInfo, WorkspaceSnapshot,
    };

    use super::*;

    fn pane(id: u64, tab: u64, state: P, since: u64) -> PaneInfo {
        PaneInfo {
            id: PaneId::new(id),
            tab: TabId::new(tab),
            name: format!("pane-{id}"),
            profile: "Claude Code".into(),
            profile_id: ProfileId::new("claude"),
            title: None,
            cwd: None,
            state,
            running: true,
            exit_code: None,
            session_id: None,
            needs_you_since: state.is_red().then_some(since),
            attention_since: AgentState::from(state).wants_attention().then_some(since),
            question: None,
            signal_lost: false,
            runs_session: true,
        }
    }

    fn tab(id: u64, project: u64, panes: &[u64]) -> TabInfo {
        TabInfo {
            id: TabId::new(id),
            project: ProjectId::new(project),
            name: format!("tab-{id}"),
            dir: "C:/Code".into(),
            branch: None,
            layout: LayoutNode::Split {
                orientation: Orientation::Horizontal,
                children: panes
                    .iter()
                    .map(|p| LayoutChild {
                        flex: 1.0,
                        node: LayoutNode::Pane(PaneId::new(*p)),
                    })
                    .collect(),
            },
            focused: PaneId::new(panes[0]),
            maximized: None,
            muted: false,
            worktree: None,
        }
    }

    const NEEDS: P = P::NeedsYou(NeedsReason::Permission);
    const FAILED: P = P::Error(ErrorReason::TurnFailed);

    /// Two projects; panes in every state, entering them at the given
    /// sequence numbers.
    fn view() -> WorkspaceView {
        let mut asking = pane(201, 20, NEEDS, 7);
        asking.question = Some(PendingQuestion::new("Permission to use Bash"));
        WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![
                ProjectInfo {
                    id: ProjectId::new(1),
                    name: "api".into(),
                    root: "C:/api".into(),
                    tabs: vec![TabId::new(10), TabId::new(11)],
                    collapsed: true,
                },
                ProjectInfo {
                    id: ProjectId::new(2),
                    name: "web".into(),
                    root: "C:/web".into(),
                    tabs: vec![TabId::new(20)],
                    collapsed: false,
                },
            ],
            tabs: vec![
                tab(10, 1, &[100, 101, 102]),
                tab(11, 1, &[110, 111]),
                tab(20, 2, &[200, 201, 202]),
            ],
            panes: vec![
                pane(100, 10, P::Done, 9),
                pane(101, 10, P::Working, 0),
                pane(102, 10, NEEDS, 8),
                pane(110, 11, FAILED, 5),
                pane(111, 11, P::Idle, 0),
                pane(200, 20, P::Done, 3),
                asking,
                pane(202, 20, FAILED, 6),
            ],
            active_tab: Some(TabId::new(10)),
            profiles: vec![],
            notifications_muted: false,
            side_bars: Default::default(),
        })
    }

    fn order(rows: &[TrayRow]) -> Vec<u64> {
        rows.iter().map(|r| r.pane.get()).collect()
    }

    #[test]
    fn rf_atn_13_the_tray_lists_needs_you_then_error_then_done_oldest_first() {
        let rows = tray_rows(&view());
        assert_eq!(order(&rows), vec![201, 102, 110, 202, 200, 100]);
        let states: Vec<AgentState> = rows.iter().map(|r| r.state).collect();
        assert_eq!(
            states,
            [
                AgentState::NeedsYou,
                AgentState::NeedsYou,
                AgentState::Error,
                AgentState::Error,
                AgentState::Done,
                AgentState::Done
            ]
        );
        assert!(
            !order(&rows).contains(&101) && !order(&rows).contains(&111),
            "working and idle panes are not listed"
        );
    }

    #[test]
    fn rf_atn_13_a_row_names_its_place_and_profile_and_says_what_it_asks() {
        let rows = tray_rows(&view());
        let first = &rows[0];
        assert_eq!(first.place, "web / tab-20 › pane-201");
        assert_eq!(first.profile, "Claude Code");
        assert_eq!(first.detail, "Permission to use Bash");
        assert_eq!(first.tab, TabId::new(20));
        assert_eq!(rows[1].detail, "Asks for permission", "no question text");
        assert_eq!(rows[2].detail, "The turn failed");
        assert_eq!(rows[4].detail, "Turn finished");
        // A folded project's panes are listed too.
        assert_eq!(rows[1].place, "api / tab-10 › pane-102");
    }

    #[test]
    fn rf_atn_13_a_pane_that_moves_on_leaves_the_tray_and_its_question_goes() {
        let mut view = view();
        assert!(view.pane_state(PaneId::new(201), P::Working));
        assert_eq!(view.pane(PaneId::new(201)).question, None);
        assert!(view.pane_state(PaneId::new(201), NEEDS));
        let rows = tray_rows(&view);
        assert_eq!(order(&rows), vec![102, 201, 110, 202, 200, 100]);
        assert_eq!(rows[1].detail, "Asks for permission");
        assert!(tray_rows(&WorkspaceView::default()).is_empty());
    }

    #[test]
    fn rf_atn_13_an_exited_pane_in_error_names_its_code() {
        let mut view = view();
        let mut info = pane(110, 11, FAILED, 5);
        info.running = false;
        info.exit_code = Some(3);
        view.pane_updated(&info);
        let rows = tray_rows(&view);
        assert_eq!(rows[2].detail, "Stopped with exit code 3");
    }

    #[test]
    fn rf_atn_13_the_cursor_wraps_and_follows_its_pane() {
        let rows = tray_rows(&view());
        assert_eq!(move_cursor(&rows, None, 1), Some(PaneId::new(201)));
        assert_eq!(move_cursor(&rows, None, -1), Some(PaneId::new(100)));
        assert_eq!(
            move_cursor(&rows, Some(PaneId::new(201)), -1),
            Some(PaneId::new(100))
        );
        assert_eq!(
            move_cursor(&rows, Some(PaneId::new(100)), 1),
            Some(PaneId::new(201))
        );
        assert_eq!(move_cursor(&[], None, 1), None);
        // The cursor's pane left: the row now at its place takes it.
        let fewer: Vec<TrayRow> = rows[1..].to_vec();
        assert_eq!(
            keep_cursor(&fewer, Some(PaneId::new(201)), 0),
            Some(PaneId::new(102))
        );
        assert_eq!(
            keep_cursor(&fewer, Some(PaneId::new(202)), 3),
            Some(PaneId::new(202))
        );
        assert_eq!(
            keep_cursor(&fewer, Some(PaneId::new(999)), 99),
            Some(PaneId::new(100))
        );
        assert_eq!(keep_cursor(&[], Some(PaneId::new(1)), 0), None);
    }

    #[test]
    fn rf_atn_13_the_scroll_target_counts_the_section_heads() {
        let rows = tray_rows(&view());
        let at = |id| list_index(&rows, PaneId::new(id));
        // Needs you: head, 201, 102; error: head, 110, 202; done: head, 200, 100.
        assert_eq!(at(201), Some(1));
        assert_eq!(at(102), Some(2));
        assert_eq!(at(110), Some(4));
        assert_eq!(at(202), Some(5));
        assert_eq!(at(200), Some(7));
        assert_eq!(at(100), Some(8));
        assert_eq!(at(999), None);
    }

    #[test]
    fn rf_atn_13_a_row_never_shows_its_question_in_debug() {
        let view = view();
        let rows = tray_rows(&view);
        assert_eq!(rows[0].detail, "Permission to use Bash");
        assert!(!format!("{rows:?}").contains("Bash"), "{rows:?}");
        assert!(!format!("{view:?}").contains("Bash"));
    }

    #[test]
    fn rf_ses_13_closing_asks_only_while_agents_work() {
        assert_eq!(close_request(0, false, false), CloseRequest::Close);
        assert_eq!(close_request(2, false, false), CloseRequest::Ask(2));
        assert_eq!(close_request(2, true, false), CloseRequest::StillAsking);
        assert_eq!(close_request(2, true, true), CloseRequest::Close);
        assert_eq!(close_request(1, false, true), CloseRequest::Close);
        // The agents finished while the question was up: closing goes on.
        assert_eq!(close_request(0, true, false), CloseRequest::Close);
    }

    #[test]
    fn rf_ses_13_the_quit_key_again_quits_but_a_held_one_keeps_asking() {
        assert_eq!(
            quit_key_request(2, false, false, false),
            CloseRequest::Ask(2)
        );
        assert_eq!(
            quit_key_request(0, false, false, false),
            CloseRequest::Close
        );
        assert_eq!(
            quit_key_request(2, true, false, false),
            CloseRequest::StillAsking,
            "a repeat of the press that opened the question"
        );
        assert_eq!(quit_key_request(2, true, true, false), CloseRequest::Close);
    }

    #[test]
    fn rf_ses_13_only_the_quit_key_coming_up_makes_the_next_press_new() {
        assert!(releases_quit_key("q", Some("q")));
        assert!(
            !releases_quit_key("a", Some("q")),
            "another key released while the quit key is held"
        );
        assert!(!releases_quit_key("q", None), "the quit command has no key");
    }

    #[test]
    fn rf_ses_13_the_question_counts_the_working_agents() {
        assert!(quit_detail(1).starts_with("1 agent is still working."));
        assert!(quit_detail(3).starts_with("3 agents are still working."));
        assert_eq!(quit_detail(0), "No agent is working any more.");
    }
}
