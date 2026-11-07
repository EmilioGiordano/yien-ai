//! Decisions the core takes per pane and per tab when it restores a snapshot.

use yien_proto::TabId;

use crate::model::{PaneRecord, ProfileId, SessionRef, SessionSnapshot};

/// How to relaunch a restored pane.
///
/// ```
/// use yien_session::ResumeAction;
/// let action = ResumeAction::Fresh { rerun_offer: None };
/// assert!(!matches!(action, ResumeAction::Resume { .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeAction {
    /// Launch the agent CLI `kind` resuming exactly this session (RN-05).
    Resume {
        /// The agent profile that owns the session; always the pane's own profile.
        kind: ProfileId,
        /// The session's exact id.
        session: SessionRef,
    },
    /// Launch the pane's profile with no session to resume.
    Fresh {
        /// For a pane without an agent, its last command, to offer for rerun after the
        /// user confirms (RF-SES-05). Never run without asking.
        rerun_offer: Option<String>,
    },
}

/// Decides how to relaunch a pane.
///
/// An agent session is resumed only when the pane has its exact id and the user did not end
/// it (RN-04, RN-05); an empty or blank id counts as none. A session whose agent kind is not the
/// pane's profile is never resumed, so one CLI's id never reaches another. Anything else
/// starts fresh.
///
/// ```
/// use yien_proto::PaneId;
/// use yien_session::{AgentRecord, PaneRecord, ProfileId, ResumeAction, SessionRef, resume_plan};
///
/// let mut pane = PaneRecord {
///     id: PaneId::new(1),
///     profile: ProfileId::new("claude"),
///     cwd: "C:/code".into(),
///     agent: Some(AgentRecord {
///         kind: ProfileId::new("claude"),
///         session_id: Some(SessionRef::new("4f1c")),
///         ended_explicitly: false,
///     }),
///     last_command: None,
///     name: None,
/// };
/// assert_eq!(
///     resume_plan(&pane),
///     ResumeAction::Resume { kind: ProfileId::new("claude"), session: SessionRef::new("4f1c") }
/// );
/// ```
pub fn resume_plan(pane: &PaneRecord) -> ResumeAction {
    match &pane.agent {
        Some(agent) => match &agent.session_id {
            Some(id)
                if !agent.ended_explicitly
                    && !id.as_str().trim().is_empty()
                    && agent.kind == pane.profile =>
            {
                ResumeAction::Resume {
                    kind: agent.kind.clone(),
                    session: id.clone(),
                }
            }
            _ => ResumeAction::Fresh { rerun_offer: None },
        },
        None => ResumeAction::Fresh {
            rerun_offer: pane.last_command.clone().filter(|c| !c.trim().is_empty()),
        },
    }
}

/// The tabs whose directory no longer exists, to warn about on restore (RF-SES-12).
///
/// `exists` is the caller's file system check, so this crate stays free of I/O.
///
/// ```
/// let snapshot = yien_session::SessionSnapshot::new(0, Vec::new());
/// assert!(yien_session::missing_dirs(&snapshot, |_| false).is_empty());
/// ```
pub fn missing_dirs(snapshot: &SessionSnapshot, exists: impl Fn(&str) -> bool) -> Vec<TabId> {
    snapshot
        .projects
        .iter()
        .flat_map(|p| &p.tabs)
        .filter(|t| !exists(&t.dir))
        .map(|t| t.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentRecord, LayoutRecord, ProjectRecord, TabRecord};
    use yien_proto::{PaneId, ProjectId};

    fn agent_pane(session: Option<&str>, ended: bool) -> PaneRecord {
        PaneRecord {
            id: PaneId::new(1),
            profile: ProfileId::new("claude"),
            cwd: "C:/code".into(),
            agent: Some(AgentRecord {
                kind: ProfileId::new("claude"),
                session_id: session.map(SessionRef::new),
                ended_explicitly: ended,
            }),
            last_command: Some("claude".into()),
            name: None,
        }
    }

    fn shell_pane(last: Option<&str>) -> PaneRecord {
        PaneRecord {
            id: PaneId::new(2),
            profile: ProfileId::new("pwsh"),
            cwd: "C:/code".into(),
            agent: None,
            last_command: last.map(str::to_owned),
            name: None,
        }
    }

    #[test]
    fn rf_ses_03_a_live_session_resumes_by_its_id() {
        assert_eq!(
            resume_plan(&agent_pane(Some("abc-123"), false)),
            ResumeAction::Resume {
                kind: ProfileId::new("claude"),
                session: SessionRef::new("abc-123")
            }
        );
    }

    #[test]
    fn rn_05_a_session_of_another_agent_is_never_resumed() {
        let mut pane = agent_pane(Some("abc-123"), false);
        pane.profile = ProfileId::new("codex");
        assert_eq!(
            resume_plan(&pane),
            ResumeAction::Fresh { rerun_offer: None }
        );
    }

    #[test]
    fn rn_04_an_ended_session_is_not_resumed() {
        assert_eq!(
            resume_plan(&agent_pane(Some("abc-123"), true)),
            ResumeAction::Fresh { rerun_offer: None }
        );
    }

    #[test]
    fn rf_ses_04_an_ended_session_without_id_starts_fresh() {
        assert_eq!(
            resume_plan(&agent_pane(None, true)),
            ResumeAction::Fresh { rerun_offer: None }
        );
    }

    #[test]
    fn rn_05_without_an_exact_id_nothing_is_resumed() {
        for session in [None, Some(""), Some(" \t ")] {
            assert_eq!(
                resume_plan(&agent_pane(session, false)),
                ResumeAction::Fresh { rerun_offer: None }
            );
        }
    }

    #[test]
    fn rf_ses_05_a_shell_pane_offers_its_last_command() {
        assert_eq!(
            resume_plan(&shell_pane(Some("cargo test"))),
            ResumeAction::Fresh {
                rerun_offer: Some("cargo test".into())
            }
        );
        for last in [None, Some(""), Some("   ")] {
            assert_eq!(
                resume_plan(&shell_pane(last)),
                ResumeAction::Fresh { rerun_offer: None }
            );
        }
    }

    #[test]
    fn rf_ses_12_tabs_with_a_missing_directory_are_listed() {
        let tab = |id: u64, dir: &str| TabRecord {
            id: TabId::new(id),
            title: "t".into(),
            dir: dir.into(),
            layout: LayoutRecord::Leaf {
                pane: PaneId::new(id),
            },
            panes: vec![PaneRecord {
                id: PaneId::new(id),
                ..shell_pane(None)
            }],
            focused: PaneId::new(id),
            branch: None,
            worktree: None,
            archived: false,
            muted: false,
            name: None,
        };
        let snapshot = SessionSnapshot::new(
            0,
            vec![ProjectRecord {
                id: ProjectId::new(1),
                name: "p".into(),
                root: "C:/code".into(),
                setup_script: None,
                collapsed: false,
                tabs: vec![tab(1, "C:/code"), tab(2, "C:/gone"), tab(3, "C:/code/wt")],
            }],
        );
        let missing = missing_dirs(&snapshot, |dir| !dir.ends_with("gone"));
        assert_eq!(missing, vec![TabId::new(2)]);
    }
}
