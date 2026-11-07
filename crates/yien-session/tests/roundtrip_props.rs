//! Any valid snapshot survives encoding and a save/load cycle unchanged.

use proptest::prelude::*;
use yien_proto::{PaneId, ProjectId, TabId};
use yien_session::{
    AgentRecord, LayoutRecord, MemStore, Orientation, PaneRecord, ProfileId, ProjectRecord,
    SessionRef, SessionSnapshot, TabRecord, deserialize, load, save, serialize, validate,
};

#[derive(Debug, Clone)]
struct PaneSeed {
    profile: String,
    cwd: String,
    agent: Option<(String, Option<String>, bool)>,
    last_command: Option<String>,
}

#[derive(Debug, Clone)]
struct TabSeed {
    title: String,
    dir: String,
    panes: Vec<PaneSeed>,
    shape: Vec<u32>,
    focus: usize,
}

type ProjectSeed = (String, String, Vec<TabSeed>);

fn text() -> impl Strategy<Value = String> {
    "\\PC{0,12}"
}

fn pane_seed() -> impl Strategy<Value = PaneSeed> {
    (
        text(),
        text(),
        proptest::option::of((text(), proptest::option::of(text()), any::<bool>())),
        proptest::option::of(text()),
    )
        .prop_map(|(profile, cwd, agent, last_command)| PaneSeed {
            profile,
            cwd,
            agent,
            last_command,
        })
}

fn tab_seed() -> impl Strategy<Value = TabSeed> {
    (
        text(),
        text(),
        prop::collection::vec(pane_seed(), 1..7),
        prop::collection::vec(any::<u32>(), 1..24),
        any::<usize>(),
    )
        .prop_map(|(title, dir, panes, shape, focus)| TabSeed {
            title,
            dir,
            panes,
            shape,
            focus,
        })
}

fn project_seed() -> impl Strategy<Value = ProjectSeed> {
    (text(), text(), prop::collection::vec(tab_seed(), 0..4))
}

/// A random tree over `panes`, in order, drawing its shape from `shape`.
fn layout(panes: &[PaneId], shape: &mut impl Iterator<Item = u32>) -> LayoutRecord {
    if let [only] = panes {
        return LayoutRecord::Leaf { pane: *only };
    }
    let mut next = || shape.next().unwrap_or(0) as usize;
    let groups = 2 + next() % (panes.len() - 1).min(3);
    let mut cuts: Vec<usize> = Vec::new();
    while cuts.len() < groups - 1 {
        let cut = 1 + next() % (panes.len() - 1);
        if !cuts.contains(&cut) {
            cuts.push(cut);
        } else if let Some(free) = (1..panes.len()).find(|c| !cuts.contains(c)) {
            cuts.push(free);
        }
    }
    cuts.sort_unstable();
    let orientation = if next() % 2 == 0 {
        Orientation::Horizontal
    } else {
        Orientation::Vertical
    };
    let weights: Vec<f32> = (0..groups).map(|_| (1 + next() % 100) as f32).collect();
    let total: f32 = weights.iter().sum();

    let mut bounds = vec![0];
    bounds.extend(cuts);
    bounds.push(panes.len());
    let children = bounds
        .windows(2)
        .map(|w| layout(&panes[w[0]..w[1]], shape))
        .collect();
    LayoutRecord::Split {
        orientation,
        children,
        ratios: weights.iter().map(|w| w / total).collect(),
    }
}

fn build((saved_at, id_base, projects): (u64, u64, Vec<ProjectSeed>)) -> SessionSnapshot {
    let mut next_id = id_base;
    let mut fresh = || {
        next_id = next_id.wrapping_add(1);
        next_id
    };
    let projects = projects
        .into_iter()
        .map(|(name, root, tabs)| ProjectRecord {
            id: ProjectId::new(fresh()),
            name,
            root,
            tabs: tabs
                .into_iter()
                .map(|seed| {
                    let panes: Vec<PaneRecord> = seed
                        .panes
                        .into_iter()
                        .map(|p| PaneRecord {
                            id: PaneId::new(fresh()),
                            profile: ProfileId::new(p.profile),
                            cwd: p.cwd,
                            agent: p.agent.map(|(kind, session, ended)| AgentRecord {
                                kind: ProfileId::new(kind),
                                session_id: session.map(SessionRef::new),
                                ended_explicitly: ended,
                            }),
                            last_command: p.last_command,
                        })
                        .collect();
                    let ids: Vec<PaneId> = panes.iter().map(|p| p.id).collect();
                    let mut shape = seed.shape.into_iter().cycle();
                    TabRecord {
                        id: TabId::new(fresh()),
                        title: seed.title,
                        dir: seed.dir,
                        layout: layout(&ids, &mut shape),
                        focused: ids[seed.focus % ids.len()],
                        panes,
                    }
                })
                .collect(),
        })
        .collect();
    SessionSnapshot::new(saved_at, projects)
}

fn snapshot() -> impl Strategy<Value = SessionSnapshot> {
    (
        any::<u64>(),
        any::<u64>(),
        prop::collection::vec(project_seed(), 0..4),
    )
        .prop_map(build)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn rf_ses_01_any_valid_snapshot_round_trips(snapshot in snapshot()) {
        prop_assert_eq!(validate(&snapshot), Ok(()));
        let bytes = serialize(&snapshot).unwrap();
        prop_assert_eq!(deserialize(&bytes).unwrap(), snapshot);
    }

    #[test]
    fn rf_ses_02_any_valid_snapshot_survives_save_and_load(snapshot in snapshot()) {
        let mut store = MemStore::new();
        save(&mut store, &snapshot).unwrap();
        let report = load(&store);
        prop_assert!(report.skipped.is_empty());
        prop_assert_eq!(report.restored.unwrap().snapshot, snapshot);
    }
}
