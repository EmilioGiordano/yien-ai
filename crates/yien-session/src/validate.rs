//! The snapshot invariants (plan §6.2), checked on load and before every save.

use std::collections::HashSet;

use yien_proto::{PaneId, ProjectId, TabId};

use crate::codec::SCHEMA_VERSION;
use crate::model::{LayoutRecord, SessionSnapshot, TabRecord};

/// How far a split's ratios may drift from summing to 1 (f32 resizes accumulate error).
pub const RATIO_EPSILON: f32 = 1e-3;

/// One broken invariant.
///
/// ```
/// let problem = yien_session::Problem::TabWithoutPanes(yien_proto::TabId::new(2));
/// assert!(problem.to_string().contains("no panes"));
/// ```
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Problem {
    /// The snapshot is not in the schema this build writes.
    #[error("schema version {found} is not the current {SCHEMA_VERSION}")]
    SchemaVersion {
        /// The version found.
        found: u32,
    },
    /// Two projects share an id.
    #[error("project id {} appears more than once", .0.get())]
    DuplicateProject(ProjectId),
    /// Two tabs share an id.
    #[error("tab id {} appears more than once", .0.get())]
    DuplicateTab(TabId),
    /// Two panes share an id, in one tab or across tabs.
    #[error("pane id {} appears more than once", .0.get())]
    DuplicatePane(PaneId),
    /// A tab has no panes.
    #[error("tab {} has no panes", .0.get())]
    TabWithoutPanes(TabId),
    /// A layout leaf names a pane the tab does not have.
    #[error("tab {}: layout leaf {} has no pane", tab.get(), pane.get())]
    LeafWithoutPane {
        /// The tab.
        tab: TabId,
        /// The pane the leaf names.
        pane: PaneId,
    },
    /// The same pane is shown by two leaves.
    #[error("tab {}: pane {} appears in the layout more than once", tab.get(), pane.get())]
    LeafRepeated {
        /// The tab.
        tab: TabId,
        /// The repeated pane.
        pane: PaneId,
    },
    /// A pane of the tab is missing from its layout.
    #[error("tab {}: pane {} is not in the layout", tab.get(), pane.get())]
    PaneOutsideLayout {
        /// The tab.
        tab: TabId,
        /// The pane left out.
        pane: PaneId,
    },
    /// A split has fewer than two children.
    #[error("tab {}: a split has fewer than two children", .0.get())]
    SplitTooSmall(TabId),
    /// A split's ratios do not match its children or do not sum to 1.
    #[error("tab {}: a split's ratios are not one positive share per child summing to 1", .0.get())]
    BadRatios(TabId),
    /// The focused pane is not one of the tab's panes.
    #[error("tab {}: focused pane {} is not in the tab", tab.get(), pane.get())]
    FocusedPaneMissing {
        /// The tab.
        tab: TabId,
        /// The focused pane.
        pane: PaneId,
    },
}

/// Checks every invariant and returns all the problems found.
///
/// The ratios are not held to `yien-layout`'s 5 % minimum: repeated splits halve a pane's
/// share, so a real layout can go below it.
///
/// ```
/// let empty = yien_session::SessionSnapshot::new(0, Vec::new());
/// assert_eq!(yien_session::validate(&empty), Ok(()));
/// ```
pub fn validate(snapshot: &SessionSnapshot) -> Result<(), Vec<Problem>> {
    let mut problems = Vec::new();
    if snapshot.schema_version != SCHEMA_VERSION {
        problems.push(Problem::SchemaVersion {
            found: snapshot.schema_version,
        });
    }

    let mut projects = HashSet::new();
    let mut tabs = HashSet::new();
    let mut panes = HashSet::new();
    for project in &snapshot.projects {
        if !projects.insert(project.id) {
            problems.push(Problem::DuplicateProject(project.id));
        }
        for tab in &project.tabs {
            if !tabs.insert(tab.id) {
                problems.push(Problem::DuplicateTab(tab.id));
            }
            for pane in &tab.panes {
                if !panes.insert(pane.id) {
                    problems.push(Problem::DuplicatePane(pane.id));
                }
            }
            check_tab(tab, &mut problems);
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn check_tab(tab: &TabRecord, problems: &mut Vec<Problem>) {
    if tab.panes.is_empty() {
        problems.push(Problem::TabWithoutPanes(tab.id));
    }
    let own: HashSet<PaneId> = tab.panes.iter().map(|p| p.id).collect();

    let mut shown = HashSet::new();
    for pane in tab.layout.panes() {
        if !own.contains(&pane) {
            problems.push(Problem::LeafWithoutPane { tab: tab.id, pane });
        }
        if !shown.insert(pane) {
            problems.push(Problem::LeafRepeated { tab: tab.id, pane });
        }
    }
    for pane in &tab.panes {
        if !shown.contains(&pane.id) {
            problems.push(Problem::PaneOutsideLayout {
                tab: tab.id,
                pane: pane.id,
            });
        }
    }

    check_splits(tab.id, &tab.layout, problems);

    if !own.contains(&tab.focused) {
        problems.push(Problem::FocusedPaneMissing {
            tab: tab.id,
            pane: tab.focused,
        });
    }
}

fn check_splits(tab: TabId, layout: &LayoutRecord, problems: &mut Vec<Problem>) {
    let LayoutRecord::Split {
        children, ratios, ..
    } = layout
    else {
        return;
    };
    if children.len() < 2 {
        problems.push(Problem::SplitTooSmall(tab));
    }
    let shares_ok = ratios.iter().all(|r| r.is_finite() && *r > 0.0);
    let sum: f32 = ratios.iter().sum();
    if ratios.len() != children.len() || !shares_ok || (sum - 1.0).abs() > RATIO_EPSILON {
        problems.push(Problem::BadRatios(tab));
    }
    for child in children {
        check_splits(tab, child, problems);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Orientation, PaneRecord, ProfileId, ProjectRecord};

    fn pane(id: u64) -> PaneRecord {
        PaneRecord {
            id: PaneId::new(id),
            profile: ProfileId::new("pwsh"),
            cwd: "C:/code".into(),
            agent: None,
            last_command: None,
        }
    }

    fn leaf(id: u64) -> LayoutRecord {
        LayoutRecord::Leaf {
            pane: PaneId::new(id),
        }
    }

    fn split(children: Vec<LayoutRecord>, ratios: Vec<f32>) -> LayoutRecord {
        LayoutRecord::Split {
            orientation: Orientation::Vertical,
            children,
            ratios,
        }
    }

    fn tab(id: u64, layout: LayoutRecord, panes: Vec<PaneRecord>, focused: u64) -> TabRecord {
        TabRecord {
            id: TabId::new(id),
            title: "t".into(),
            dir: "C:/code".into(),
            layout,
            panes,
            focused: PaneId::new(focused),
        }
    }

    fn snapshot(projects: Vec<(u64, Vec<TabRecord>)>) -> SessionSnapshot {
        SessionSnapshot::new(
            0,
            projects
                .into_iter()
                .map(|(id, tabs)| ProjectRecord {
                    id: ProjectId::new(id),
                    name: "p".into(),
                    root: "C:/code".into(),
                    tabs,
                })
                .collect(),
        )
    }

    fn two_pane_tab(id: u64, a: u64, b: u64) -> TabRecord {
        tab(
            id,
            split(vec![leaf(a), leaf(b)], vec![0.5, 0.5]),
            vec![pane(a), pane(b)],
            a,
        )
    }

    fn problems(s: &SessionSnapshot) -> Vec<Problem> {
        validate(s).err().unwrap_or_default()
    }

    #[test]
    fn rf_ses_01_a_well_formed_snapshot_is_valid() {
        let s = snapshot(vec![
            (1, vec![two_pane_tab(1, 1, 2), two_pane_tab(2, 3, 4)]),
            (2, vec![tab(3, leaf(5), vec![pane(5)], 5)]),
        ]);
        assert_eq!(validate(&s), Ok(()));
    }

    #[test]
    fn duplicate_ids_are_reported_across_the_snapshot() {
        let s = snapshot(vec![
            (1, vec![two_pane_tab(1, 1, 2)]),
            (1, vec![two_pane_tab(1, 2, 3)]),
        ]);
        let p = problems(&s);
        assert!(p.contains(&Problem::DuplicateProject(ProjectId::new(1))));
        assert!(p.contains(&Problem::DuplicateTab(TabId::new(1))));
        assert!(p.contains(&Problem::DuplicatePane(PaneId::new(2))));
    }

    #[test]
    fn a_leaf_without_a_pane_is_reported() {
        let s = snapshot(vec![(
            1,
            vec![tab(
                1,
                split(vec![leaf(1), leaf(9)], vec![0.5, 0.5]),
                vec![pane(1)],
                1,
            )],
        )]);
        assert!(problems(&s).contains(&Problem::LeafWithoutPane {
            tab: TabId::new(1),
            pane: PaneId::new(9)
        }));
    }

    #[test]
    fn a_pane_outside_the_layout_and_a_repeated_leaf_are_reported() {
        let s = snapshot(vec![(
            1,
            vec![tab(
                1,
                split(vec![leaf(1), leaf(1)], vec![0.5, 0.5]),
                vec![pane(1), pane(2)],
                1,
            )],
        )]);
        let p = problems(&s);
        assert!(p.contains(&Problem::LeafRepeated {
            tab: TabId::new(1),
            pane: PaneId::new(1)
        }));
        assert!(p.contains(&Problem::PaneOutsideLayout {
            tab: TabId::new(1),
            pane: PaneId::new(2)
        }));
    }

    #[test]
    fn a_missing_focused_pane_is_reported() {
        let s = snapshot(vec![(1, vec![tab(1, leaf(1), vec![pane(1)], 7)])]);
        assert_eq!(
            problems(&s),
            vec![Problem::FocusedPaneMissing {
                tab: TabId::new(1),
                pane: PaneId::new(7)
            }]
        );
    }

    #[test]
    fn a_tab_without_panes_is_reported() {
        let s = snapshot(vec![(1, vec![tab(1, leaf(1), Vec::new(), 1)])]);
        assert!(problems(&s).contains(&Problem::TabWithoutPanes(TabId::new(1))));
    }

    #[test]
    fn bad_splits_are_reported() {
        let id = TabId::new(1);
        let cases = [
            (split(vec![leaf(1)], vec![1.0]), Problem::SplitTooSmall(id)),
            (
                split(vec![leaf(1), leaf(2)], vec![1.0]),
                Problem::BadRatios(id),
            ),
            (
                split(vec![leaf(1), leaf(2)], vec![0.5, 0.6]),
                Problem::BadRatios(id),
            ),
            (
                split(vec![leaf(1), leaf(2)], vec![1.5, -0.5]),
                Problem::BadRatios(id),
            ),
            (
                split(vec![leaf(1), leaf(2)], vec![f32::NAN, 0.5]),
                Problem::BadRatios(id),
            ),
        ];
        for (layout, expected) in cases {
            let panes = layout.panes().iter().map(|p| pane(p.get())).collect();
            let s = snapshot(vec![(1, vec![tab(1, layout, panes, 1)])]);
            assert!(problems(&s).contains(&expected), "{expected:?}");
        }
    }

    #[test]
    fn nested_splits_are_checked_and_small_shares_are_allowed() {
        let inner = split(vec![leaf(2), leaf(3)], vec![0.03, 0.97]);
        let ok = split(vec![leaf(1), inner], vec![0.4, 0.6]);
        let s = snapshot(vec![(
            1,
            vec![tab(1, ok, vec![pane(1), pane(2), pane(3)], 3)],
        )]);
        assert_eq!(validate(&s), Ok(()));

        let inner = split(vec![leaf(2), leaf(3)], vec![0.3, 0.3]);
        let bad = split(vec![leaf(1), inner], vec![0.4, 0.6]);
        let s = snapshot(vec![(
            1,
            vec![tab(1, bad, vec![pane(1), pane(2), pane(3)], 3)],
        )]);
        assert_eq!(problems(&s), vec![Problem::BadRatios(TabId::new(1))]);
    }

    #[test]
    fn a_snapshot_in_another_schema_is_reported() {
        let mut s = snapshot(Vec::new());
        s.schema_version = 0;
        assert_eq!(problems(&s), vec![Problem::SchemaVersion { found: 0 }]);
    }
}
