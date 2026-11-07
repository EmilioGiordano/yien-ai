//! Snapshots shared by the integration tests.

#![allow(dead_code)]

use yien_proto::{PaneId, ProjectId, TabId};
use yien_session::{
    AgentRecord, LayoutRecord, Orientation, PaneRecord, ProfileId, ProjectRecord, Selection,
    SessionRef, SessionSnapshot, TabRecord,
};

/// [`basic`] with every optional field of plan §6.1 filled in.
pub fn sample(saved_at: u64) -> SessionSnapshot {
    let mut s = basic(saved_at);
    s.selected = Selection {
        project: Some(ProjectId::new(10)),
        tab: Some(TabId::new(21)),
    };
    let project = &mut s.projects[0];
    project.setup_script = Some("scripts/setup.ps1".into());
    project.tabs[0].branch = Some("main".into());
    project.tabs[0].panes[1].name = Some("build".into());
    let other = &mut project.tabs[1];
    other.branch = Some("feat/other".into());
    other.worktree = Some("C:/code/other".into());
    other.archived = true;
    s
}

/// One project with a split tab (an agent pane beside a shell) and a single-pane tab,
/// with the optional fields of plan §6.1 left at their defaults.
pub fn basic(saved_at: u64) -> SessionSnapshot {
    let agent = PaneRecord {
        id: PaneId::new(1),
        profile: ProfileId::new("claude"),
        cwd: "C:/code/yien".into(),
        agent: Some(AgentRecord {
            kind: ProfileId::new("claude"),
            session_id: Some(SessionRef::new("0b7e2f6a-41c3")),
            ended_explicitly: false,
        }),
        last_command: None,
        name: None,
    };
    let shell = PaneRecord {
        id: PaneId::new(2),
        profile: ProfileId::new("pwsh"),
        cwd: "C:/code/yien/crates".into(),
        agent: None,
        last_command: Some("cargo test".into()),
        name: None,
    };
    let ended = PaneRecord {
        id: PaneId::new(3),
        profile: ProfileId::new("codex"),
        cwd: "C:/code/other".into(),
        agent: Some(AgentRecord {
            kind: ProfileId::new("codex"),
            session_id: Some(SessionRef::new("rollout-9")),
            ended_explicitly: true,
        }),
        last_command: None,
        name: None,
    };
    SessionSnapshot::new(
        saved_at,
        vec![ProjectRecord {
            id: ProjectId::new(10),
            name: "yien".into(),
            root: "C:/code/yien".into(),
            setup_script: None,
            tabs: vec![
                TabRecord {
                    id: TabId::new(20),
                    title: "main".into(),
                    dir: "C:/code/yien".into(),
                    layout: LayoutRecord::Split {
                        orientation: Orientation::Horizontal,
                        children: vec![
                            LayoutRecord::Leaf { pane: agent.id },
                            LayoutRecord::Leaf { pane: shell.id },
                        ],
                        ratios: vec![0.625, 0.375],
                    },
                    focused: shell.id,
                    branch: None,
                    worktree: None,
                    archived: false,
                    panes: vec![agent, shell],
                },
                TabRecord {
                    id: TabId::new(21),
                    title: "other".into(),
                    dir: "C:/code/other".into(),
                    layout: LayoutRecord::Leaf { pane: ended.id },
                    focused: ended.id,
                    branch: None,
                    worktree: None,
                    archived: false,
                    panes: vec![ended],
                },
            ],
        }],
    )
}
