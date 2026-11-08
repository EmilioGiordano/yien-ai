//! Calling the user back when an agent needs them and they are looking
//! elsewhere (RF-ATN-09, -10, -11, -16): when to toast and flash, what the
//! toast says and the taskbar badge's count. The OS calls sit behind
//! [`OsAttention`], which the binary implements.
//!
//! The window's red mark, counter and badge never depend on any of this:
//! muting, a refused toast or the OS hiding it change nothing else.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use yien_proto::{NeedsReason, PaneId};

use crate::model::WorkspaceView;

/// How long the first new "needs you" waits for others, so a burst gives one
/// toast.
pub(crate) const COALESCE: Duration = Duration::from_secs(1);

const TITLE_MAX: usize = 64;
const BODY_MAX: usize = 160;
/// Prefix of a toast's `launch`, followed by the pane's id.
const LAUNCH_PANE: &str = "pane:";
/// The tag of the toast for a burst of panes.
const BURST_TAG: &str = "burst";

/// What a toast says, and what its click hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastText {
    /// The first line.
    pub title: String,
    /// The line under it.
    pub body: String,
    /// Handed back to the activation callback when the toast is clicked.
    pub launch: String,
    /// A toast replaces the earlier one with the same tag: one per pane, and
    /// one for bursts.
    pub tag: String,
}

/// The OS side: a system toast, the taskbar badge and a taskbar flash. Each
/// call is best effort; an implementation logs its own failures and never
/// panics.
///
/// ```
/// use yien_ui::{OsAttention, ToastText};
///
/// struct Quiet;
/// impl OsAttention for Quiet {
///     fn attach(&self, _hwnd: isize) {}
///     fn toast(&self, _toast: &ToastText) {}
///     fn badge(&self, _count: usize) {}
///     fn flash(&self) {}
///     fn on_activate(&self, _callback: Box<dyn Fn(String) + Send + Sync>) {}
/// }
/// ```
pub trait OsAttention {
    /// The window the badge and the flash apply to (a Win32 `HWND`).
    fn attach(&self, hwnd: isize);
    /// Shows a toast, replacing the previous one.
    fn toast(&self, toast: &ToastText);
    /// Shows `count` on the taskbar button; 0 clears it.
    fn badge(&self, count: usize);
    /// Flashes the taskbar button until the window comes forward.
    fn flash(&self);
    /// Runs `callback` with the toast's `launch` when the user clicks it, on
    /// any thread.
    fn on_activate(&self, callback: Box<dyn Fn(String) + Send + Sync>);
}

/// One pane that needs the user, as the decision sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub pane: PaneId,
    /// Toasts and flashes are muted for it (RF-ATN-11).
    pub muted: bool,
}

/// Decides when to toast: once per transition into "needs you", never while
/// the window has focus, never for a muted pane, and one toast for every
/// pane that arrives within [`COALESCE`] of the first.
#[derive(Debug, Default)]
pub(crate) struct Notifier {
    /// The panes that needed the user at the last look.
    seen: BTreeSet<PaneId>,
    /// New ones waiting for the coalescing window to close.
    pending: Vec<PaneId>,
    closes_at: Option<Instant>,
}

impl Notifier {
    /// Takes the panes that need the user now. Returns when to call
    /// [`flush`](Self::flush) if this opened a coalescing window.
    pub(crate) fn observe(
        &mut self,
        waiting: &[Waiting],
        focused: bool,
        now: Instant,
    ) -> Option<Instant> {
        let fresh: Vec<Waiting> = waiting
            .iter()
            .filter(|w| !self.seen.contains(&w.pane))
            .copied()
            .collect();
        self.seen = waiting.iter().map(|w| w.pane).collect();
        if focused {
            self.pending.clear();
            self.closes_at = None;
            return None;
        }
        self.pending
            .retain(|p| waiting.iter().any(|w| w.pane == *p && !w.muted));
        for w in fresh.iter().filter(|w| !w.muted) {
            self.pending.push(w.pane);
        }
        if self.pending.is_empty() || self.closes_at.is_some() {
            return None;
        }
        let at = now + COALESCE;
        self.closes_at = Some(at);
        Some(at)
    }

    /// The panes to toast for, oldest first, once the coalescing window has
    /// closed; empty before that, or when nothing is left to say.
    pub(crate) fn flush(&mut self, focused: bool, now: Instant) -> Vec<PaneId> {
        match self.closes_at {
            Some(at) if now >= at => {}
            _ => return Vec::new(),
        }
        self.closes_at = None;
        let pending = std::mem::take(&mut self.pending);
        if focused { Vec::new() } else { pending }
    }
}

/// What a toast needs to know about one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToastPane {
    pub pane: PaneId,
    /// The agent's profile name: `Claude Code`.
    pub agent: String,
    pub tab: String,
    pub project: String,
    pub reason: Option<NeedsReason>,
}

/// The toast for `panes`, the one waiting longest first. `None` for none.
pub(crate) fn toast_text(panes: &[ToastPane]) -> Option<ToastText> {
    let first = panes.first()?;
    let (title, body, tag) = match panes {
        [one] => {
            let agent = if one.agent.is_empty() {
                "An agent"
            } else {
                &one.agent
            };
            let place = format!("{} · {}", one.tab, one.project);
            let body = match one.reason.map(reason_text) {
                Some(why) => format!("{place}: {why}"),
                None => place,
            };
            (
                format!("{agent} needs you"),
                body,
                format!("pane-{}", one.pane.get()),
            )
        }
        _ => {
            let mut tabs: Vec<&str> = Vec::new();
            for p in panes {
                if !tabs.contains(&p.tab.as_str()) {
                    tabs.push(&p.tab);
                }
            }
            (
                format!("{} agents need you", panes.len()),
                tabs.join(", "),
                BURST_TAG.to_owned(),
            )
        }
    };
    Some(ToastText {
        title: truncate(&title, TITLE_MAX),
        body: truncate(&body, BODY_MAX),
        launch: format!("{LAUNCH_PANE}{}", first.pane.get()),
        tag,
    })
}

fn reason_text(reason: NeedsReason) -> &'static str {
    match reason {
        NeedsReason::Permission => "asks for permission",
        NeedsReason::Question => "has a question",
        NeedsReason::Blocked => "is blocked",
        NeedsReason::ScreenRule => "is waiting for you",
    }
}

/// `text` cut to at most `max` characters, ending in `…` when cut.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

/// The pane a toast's `launch` names, if it is one of ours.
pub(crate) fn launch_pane(launch: &str) -> Option<PaneId> {
    launch
        .strip_prefix(LAUNCH_PANE)?
        .parse::<u64>()
        .ok()
        .map(PaneId::new)
}

/// The decision wired to the OS: what the window calls as its state and
/// focus change.
pub(crate) struct Alerts {
    os: Rc<dyn OsAttention>,
    notifier: Notifier,
    badge: Option<usize>,
}

impl Alerts {
    pub(crate) fn new(os: Rc<dyn OsAttention>) -> Self {
        Self {
            os,
            notifier: Notifier::default(),
            badge: None,
        }
    }

    /// The workspace or the focus changed. Updates the badge when its count
    /// moved and returns when to [`flush`](Self::flush), if a toast may be
    /// due.
    pub(crate) fn update(
        &mut self,
        view: &WorkspaceView,
        focused: bool,
        now: Instant,
    ) -> Option<Instant> {
        let waiting = view.waiting_panes();
        if self.badge != Some(waiting.len()) {
            self.badge = Some(waiting.len());
            self.os.badge(waiting.len());
        }
        let waiting: Vec<Waiting> = waiting
            .into_iter()
            .map(|pane| Waiting {
                pane,
                muted: view.alerts_muted(pane),
            })
            .collect();
        self.notifier.observe(&waiting, focused, now)
    }

    /// Sets the badge again, for a taskbar button that was recreated.
    pub(crate) fn refresh_badge(&self) {
        if let Some(count) = self.badge {
            self.os.badge(count);
        }
    }

    /// Toasts and flashes once the coalescing window has closed. Panes that
    /// stopped waiting since are left out.
    pub(crate) fn flush(&mut self, view: &WorkspaceView, focused: bool, now: Instant) {
        let due = self.notifier.flush(focused, now);
        let waiting = view.waiting_panes();
        let panes: Vec<ToastPane> = waiting
            .iter()
            .filter(|p| due.contains(p))
            .filter_map(|p| toast_pane(view, *p))
            .collect();
        if let Some(toast) = toast_text(&panes) {
            self.os.toast(&toast);
            self.os.flash();
        }
    }
}

fn toast_pane(view: &WorkspaceView, pane: PaneId) -> Option<ToastPane> {
    let tab = view.tab_of(pane)?;
    let info = view.pane(pane);
    Some(ToastPane {
        pane,
        agent: info.profile.clone(),
        tab: tab.name.clone(),
        project: view
            .project(tab.project)
            .map(|p| p.name.clone())
            .unwrap_or_default(),
        reason: info.needs_reason,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use yien_proto::{
        AgentState as P, LayoutNode, PaneInfo, ProfileId, ProjectId, ProjectInfo, TabId, TabInfo,
        WorkspaceSnapshot,
    };

    use super::*;
    use crate::model::AgentState;

    fn w(pane: u64) -> Waiting {
        Waiting {
            pane: PaneId::new(pane),
            muted: false,
        }
    }

    fn ids(panes: &[u64]) -> Vec<PaneId> {
        panes.iter().map(|p| PaneId::new(*p)).collect()
    }

    #[test]
    fn rf_atn_09_one_toast_per_transition_into_needs_you() {
        let t0 = Instant::now();
        let mut n = Notifier::default();
        let at = n.observe(&[w(1)], false, t0).expect("a window opens");
        assert_eq!(at, t0 + COALESCE);
        assert!(
            n.flush(false, t0).is_empty(),
            "not before the window closes"
        );
        assert_eq!(n.flush(false, at), ids(&[1]));
        // Still waiting: nothing more, however often it is looked at.
        assert_eq!(n.observe(&[w(1)], false, at), None);
        assert!(n.flush(false, at + COALESCE * 5).is_empty());
        // It clears and comes back: a new transition, a new toast.
        assert_eq!(n.observe(&[], false, at), None);
        let again = n.observe(&[w(1)], false, at).expect("a new window");
        assert_eq!(n.flush(false, again), ids(&[1]));
    }

    #[test]
    fn rf_atn_09_no_toast_while_the_window_has_focus() {
        let t0 = Instant::now();
        let mut n = Notifier::default();
        assert_eq!(n.observe(&[w(1)], true, t0), None);
        // Losing focus later does not toast for a pane already seen.
        assert_eq!(n.observe(&[w(1)], false, t0), None);
        // Focus coming back inside the window cancels the toast.
        let at = n.observe(&[w(1), w(2)], false, t0).expect("window");
        assert_eq!(n.observe(&[w(1), w(2)], true, t0), None);
        assert!(n.flush(true, at).is_empty());
        assert!(n.flush(false, at).is_empty());
    }

    #[test]
    fn rf_atn_09_focus_at_flush_time_drops_the_toast() {
        let t0 = Instant::now();
        let mut n = Notifier::default();
        let at = n.observe(&[w(1)], false, t0).expect("window");
        assert!(n.flush(true, at).is_empty());
    }

    #[test]
    fn rf_atn_09_a_burst_within_a_second_gives_one_toast() {
        let t0 = Instant::now();
        let mut n = Notifier::default();
        let at = n.observe(&[w(1)], false, t0).expect("window");
        let half = t0 + COALESCE / 2;
        assert_eq!(n.observe(&[w(1), w(2)], false, half), None);
        assert_eq!(n.observe(&[w(1), w(2), w(3)], false, half), None);
        assert_eq!(n.flush(false, at), ids(&[1, 2, 3]));
        // One after the window closed starts its own.
        let later = n.observe(&[w(1), w(2), w(3), w(4)], false, at);
        assert_eq!(later, Some(at + COALESCE));
    }

    #[test]
    fn rf_atn_11_unmuting_inside_a_window_toasts_only_new_arrivals() {
        let t0 = Instant::now();
        let mut n = Notifier::default();
        let muted = |pane| Waiting {
            pane: PaneId::new(pane),
            muted: true,
        };
        // Muted on arrival, unmuted while a window is open for another pane.
        assert_eq!(n.observe(&[muted(1)], false, t0), None);
        let at = n.observe(&[muted(1), w(2)], false, t0).expect("window");
        assert_eq!(n.observe(&[w(1), w(2)], false, t0), None);
        assert_eq!(n.flush(false, at), ids(&[2]));
        // Muted and unmuted again inside a window: its toast is not revived.
        let at = n.observe(&[w(1), w(2), w(3)], false, at).expect("window");
        n.observe(&[w(1), w(2), muted(3)], false, at);
        n.observe(&[w(1), w(2), w(3)], false, at);
        assert!(n.flush(false, at).is_empty());
    }

    #[test]
    fn rf_atn_09_a_pane_closed_inside_the_window_is_not_toasted() {
        let os = Rc::new(FakeOs::default());
        let mut alerts = Alerts::new(os.clone());
        let t0 = Instant::now();
        let at = alerts
            .update(&view(&[(1, NEEDS), (2, P::Idle)]), false, t0)
            .expect("flush due");
        let closed = view(&[(2, P::Idle)]);
        alerts.update(&closed, false, t0);
        alerts.flush(&closed, false, at);
        assert_eq!(*os.calls.borrow(), ["badge 1", "badge 0"]);
    }

    #[test]
    fn rf_atn_09_a_pane_that_clears_inside_the_window_is_not_toasted() {
        let t0 = Instant::now();
        let mut n = Notifier::default();
        let at = n.observe(&[w(1)], false, t0).expect("window");
        n.observe(&[], false, t0);
        assert!(n.flush(false, at).is_empty());
    }

    #[test]
    fn rf_atn_11_muted_panes_never_toast() {
        let t0 = Instant::now();
        let mut n = Notifier::default();
        let muted = Waiting {
            pane: PaneId::new(1),
            muted: true,
        };
        assert_eq!(n.observe(&[muted], false, t0), None);
        let at = n.observe(&[muted, w(2)], false, t0).expect("window");
        assert_eq!(n.flush(false, at), ids(&[2]));
        // Muted after it arrived but before the toast: dropped.
        let at = n.observe(&[muted, w(2), w(3)], false, at).expect("window");
        let now_muted = Waiting {
            pane: PaneId::new(3),
            muted: true,
        };
        n.observe(&[muted, w(2), now_muted], false, at);
        assert!(n.flush(false, at).is_empty());
    }

    fn tp(pane: u64, agent: &str, tab: &str, reason: Option<NeedsReason>) -> ToastPane {
        ToastPane {
            pane: PaneId::new(pane),
            agent: agent.into(),
            tab: tab.into(),
            project: "yien".into(),
            reason,
        }
    }

    #[test]
    fn rf_atn_09_toast_text_names_the_agent_tab_project_and_reason() {
        let toast = toast_text(&[tp(7, "Claude Code", "Tab 1", Some(NeedsReason::Permission))])
            .expect("toast");
        assert_eq!(toast.title, "Claude Code needs you");
        assert_eq!(toast.body, "Tab 1 · yien: asks for permission");
        assert_eq!(launch_pane(&toast.launch), Some(PaneId::new(7)));
        assert_eq!(toast.tag, "pane-7", "each pane keeps its own toast");
        let toast = toast_text(&[tp(7, "", "Tab 1", None)]).expect("toast");
        assert_eq!(toast.title, "An agent needs you");
        assert_eq!(toast.body, "Tab 1 · yien");
        assert_eq!(toast_text(&[]), None);
    }

    #[test]
    fn rf_atn_09_a_burst_says_how_many_and_opens_the_oldest() {
        let toast = toast_text(&[
            tp(3, "Claude Code", "api", None),
            tp(1, "Claude Code", "web", None),
            tp(2, "Codex", "api", None),
        ])
        .expect("toast");
        assert_eq!(toast.title, "3 agents need you");
        assert_eq!(toast.body, "api, web");
        assert_eq!(launch_pane(&toast.launch), Some(PaneId::new(3)));
        assert_eq!(toast.tag, BURST_TAG);
    }

    #[test]
    fn rf_atn_09_toast_text_is_truncated_by_characters() {
        let long = "ñ".repeat(500);
        let toast = toast_text(&[tp(1, &long, &long, None)]).expect("toast");
        assert_eq!(toast.title.chars().count(), TITLE_MAX);
        assert!(toast.title.ends_with('…'));
        assert_eq!(toast.body.chars().count(), BODY_MAX);
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("ab   cdef", 5), "ab…");
    }

    #[test]
    fn launch_strings_that_are_not_ours_name_no_pane() {
        assert_eq!(launch_pane(""), None);
        assert_eq!(launch_pane("pane:x"), None);
        assert_eq!(launch_pane("tab:3"), None);
        assert_eq!(launch_pane("pane:42"), Some(PaneId::new(42)));
    }

    /// Records every OS call.
    #[derive(Default)]
    struct FakeOs {
        calls: RefCell<Vec<String>>,
    }

    impl OsAttention for FakeOs {
        fn attach(&self, hwnd: isize) {
            self.calls.borrow_mut().push(format!("attach {hwnd}"));
        }
        fn toast(&self, toast: &ToastText) {
            self.calls
                .borrow_mut()
                .push(format!("toast {} | {}", toast.title, toast.body));
        }
        fn badge(&self, count: usize) {
            self.calls.borrow_mut().push(format!("badge {count}"));
        }
        fn flash(&self) {
            self.calls.borrow_mut().push("flash".into());
        }
        fn on_activate(&self, _callback: Box<dyn Fn(String) + Send + Sync>) {}
    }

    fn view(states: &[(u64, P)]) -> WorkspaceView {
        let tab = |id: u64| TabInfo {
            id: TabId::new(id),
            project: ProjectId::new(1),
            name: format!("Tab {id}"),
            dir: "C:/yien".into(),
            branch: None,
            layout: LayoutNode::Pane(PaneId::new(id)),
            focused: PaneId::new(id),
            maximized: None,
            muted: false,
            worktree: None,
        };
        WorkspaceView::from_snapshot(&WorkspaceSnapshot {
            projects: vec![ProjectInfo {
                id: ProjectId::new(1),
                name: "yien".into(),
                root: "C:/yien".into(),
                tabs: states.iter().map(|(p, _)| TabId::new(*p)).collect(),
                collapsed: false,
            }],
            tabs: states.iter().map(|(id, _)| tab(*id)).collect(),
            panes: states
                .iter()
                .map(|(id, state)| PaneInfo {
                    id: PaneId::new(*id),
                    tab: TabId::new(*id),
                    name: "claude".into(),
                    profile: "Claude Code".into(),
                    profile_id: ProfileId::new("claude"),
                    title: None,
                    cwd: None,
                    state: *state,
                    running: true,
                    exit_code: None,
                    session_id: None,
                    needs_you_since: state.is_red().then_some(*id),
                    signal_lost: false,
                    runs_session: true,
                })
                .collect(),
            active_tab: Some(TabId::new(states[0].0)),
            profiles: vec![],
            notifications_muted: false,
        })
    }

    const NEEDS: P = P::NeedsYou(NeedsReason::Question);

    #[test]
    fn rf_atn_10_the_badge_follows_the_count_and_clears_at_zero() {
        let os = Rc::new(FakeOs::default());
        let mut alerts = Alerts::new(os.clone());
        let t0 = Instant::now();
        alerts.update(&view(&[(1, P::Idle)]), true, t0);
        alerts.update(&view(&[(1, NEEDS), (2, NEEDS)]), true, t0);
        alerts.update(&view(&[(1, NEEDS), (2, NEEDS)]), true, t0);
        alerts.update(&view(&[(1, P::Working), (2, P::Idle)]), true, t0);
        assert_eq!(*os.calls.borrow(), ["badge 0", "badge 2", "badge 0"]);
    }

    #[test]
    fn rf_atn_09_unfocused_needs_you_toasts_and_flashes_once() {
        let os = Rc::new(FakeOs::default());
        let mut alerts = Alerts::new(os.clone());
        let t0 = Instant::now();
        alerts.update(&view(&[(1, P::Working)]), false, t0);
        let waiting = view(&[(1, NEEDS)]);
        let at = alerts.update(&waiting, false, t0).expect("flush due");
        alerts.flush(&waiting, false, at);
        alerts.update(&waiting, false, at);
        alerts.flush(&waiting, false, at + COALESCE);
        assert_eq!(
            *os.calls.borrow(),
            [
                "badge 0",
                "badge 1",
                "toast Claude Code needs you | Tab 1 · yien: has a question",
                "flash",
            ]
        );
    }

    #[test]
    fn rf_atn_16_muting_stops_toasts_and_flashes_but_not_the_mark_counter_or_badge() {
        let t0 = Instant::now();
        let mut all = view(&[(1, NEEDS)]);
        all.set_notifications_muted(true);
        let mut one = view(&[(1, NEEDS), (2, P::Idle)]);
        one.tab_updated(&TabInfo {
            id: TabId::new(1),
            project: ProjectId::new(1),
            name: "Tab 1".into(),
            dir: "C:/yien".into(),
            branch: None,
            layout: LayoutNode::Pane(PaneId::new(1)),
            focused: PaneId::new(1),
            maximized: None,
            muted: true,
            worktree: None,
        });
        assert!(one.alerts_muted(PaneId::new(1)) && !one.alerts_muted(PaneId::new(2)));
        for muted in [all, one] {
            assert_eq!(muted.needs_you_panes(), 1);
            assert_eq!(muted.pane(PaneId::new(1)).state, AgentState::NeedsYou);
            let os = Rc::new(FakeOs::default());
            let mut alerts = Alerts::new(os.clone());
            assert_eq!(alerts.update(&muted, false, t0), None);
            alerts.flush(&muted, false, t0 + COALESCE);
            assert_eq!(*os.calls.borrow(), ["badge 1"]);
        }
    }

    #[test]
    fn a_pane_that_cleared_before_the_flush_is_not_toasted_but_the_badge_moved() {
        let os = Rc::new(FakeOs::default());
        let mut alerts = Alerts::new(os.clone());
        let t0 = Instant::now();
        let at = alerts
            .update(&view(&[(1, NEEDS)]), false, t0)
            .expect("flush due");
        let cleared = view(&[(1, P::Working)]);
        alerts.flush(&cleared, false, at);
        assert_eq!(*os.calls.borrow(), ["badge 1"]);
    }
}
