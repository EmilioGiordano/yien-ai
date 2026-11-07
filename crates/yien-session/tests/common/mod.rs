//! Snapshots shared by the integration tests.

#![allow(dead_code)]

use yien_proto::{PaneId, ProjectId, TabId};
use yien_session::{
    AgentRecord, LayoutRecord, Orientation, PaneRecord, ProfileId, ProjectRecord, SessionRef,
    SessionSnapshot, TabRecord,
};

/// One project with a split tab (an agent pane beside a shell) and a single-pane tab.
pub fn sample(saved_at: u64) -> SessionSnapshot {
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
    };
    let shell = PaneRecord {
        id: PaneId::new(2),
        profile: ProfileId::new("pwsh"),
        cwd: "C:/code/yien/crates".into(),
        agent: None,
        last_command: Some("cargo test".into()),
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
    };
    SessionSnapshot::new(
        saved_at,
        vec![ProjectRecord {
            id: ProjectId::new(10),
            name: "yien".into(),
            root: "C:/code/yien".into(),
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
                    panes: vec![agent, shell],
                },
                TabRecord {
                    id: TabId::new(21),
                    title: "other".into(),
                    dir: "C:/code/other".into(),
                    layout: LayoutRecord::Leaf { pane: ended.id },
                    focused: ended.id,
                    panes: vec![ended],
                },
            ],
        }],
    )
}
