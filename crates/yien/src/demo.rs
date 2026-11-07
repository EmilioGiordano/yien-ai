//! Sample projects shown until the core fills the view: the prototype's data.

use yien_layout::{Orientation, Tree};
use yien_proto::{PaneId, ProjectId, TabId};
use yien_ui::{AgentState, PaneView, ProjectView, TabView, ViewError, WorkspaceView};

struct Ids {
    tab: u64,
    pane: u64,
}

impl Ids {
    /// A tab whose panes sit side by side, the first one focused.
    fn tab(
        &mut self,
        name: &str,
        dir: &str,
        branch: Option<&str>,
        panes: &[(&str, &str, AgentState)],
    ) -> TabView {
        self.tab += 1;
        let mut layout = Tree::new();
        let mut leaf = 0;
        let mut views = Vec::new();
        for (i, (pane_name, profile, state)) in panes.iter().enumerate() {
            if i > 0 {
                leaf = layout.split(leaf, Orientation::Horizontal).unwrap_or(leaf);
            }
            self.pane += 1;
            views.push(PaneView {
                id: PaneId::new(self.pane),
                leaf,
                name: (*pane_name).to_owned(),
                profile: (*profile).to_owned(),
                state: *state,
            });
        }
        TabView {
            id: TabId::new(self.tab),
            name: name.to_owned(),
            dir: dir.to_owned(),
            branch: branch.map(str::to_owned),
            layout,
            focused: views.first().map_or(PaneId::new(0), |p| p.id),
            panes: views,
        }
    }
}

fn project(id: u64, name: &str, root: &str, tabs: Vec<TabView>) -> ProjectView {
    ProjectView {
        id: ProjectId::new(id),
        name: name.to_owned(),
        root: root.to_owned(),
        tabs,
    }
}

pub(crate) fn workspace() -> Result<WorkspaceView, ViewError> {
    use AgentState::{Done, Error, Idle, NeedsYou, Working};
    let mut ids = Ids { tab: 0, pane: 0 };
    let website = r"C:\Code\yien-website";
    let foothy = r"C:\Code\foot-terminal";
    let zenkai = r"C:\Code\Zenkai";
    WorkspaceView::new(vec![
        project(
            1,
            "yien-website",
            website,
            vec![
                ids.tab(
                    "hero-knot",
                    r".yien\worktrees\hero-knot",
                    Some("hero/knot"),
                    &[
                        ("claude", "Claude Code", NeedsYou),
                        ("astro dev", "Shell", Idle),
                    ],
                ),
                ids.tab(
                    "h1-copy",
                    r".yien\worktrees\h1-copy",
                    Some("copy/h1"),
                    &[("codex", "Codex", Working)],
                ),
                ids.tab(
                    "main",
                    website,
                    Some("main"),
                    &[("pwsh", "PowerShell", Idle)],
                ),
            ],
        ),
        project(
            2,
            "foothy-terminal",
            foothy,
            vec![
                ids.tab(
                    "prompt-chip",
                    r".yien\worktrees\prompt-chip",
                    Some("prompt-chip-live-recolour"),
                    &[("claude", "Claude Code", Done)],
                ),
                ids.tab(
                    "scrollback-search",
                    r".yien\worktrees\scrollback-search",
                    Some("feat/scrollback-search"),
                    &[
                        ("claude", "Claude Code", NeedsYou),
                        ("cargo test", "Shell", Idle),
                    ],
                ),
            ],
        ),
        project(
            3,
            "zenkai",
            zenkai,
            vec![
                ids.tab(
                    "auth-fix",
                    r".yien\worktrees\auth-fix",
                    Some("fix/session-expiry"),
                    &[("claude", "Claude Code", Error)],
                ),
                ids.tab("docs", zenkai, None, &[("opencode", "OpenCode", Idle)]),
            ],
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_workspace_is_valid() {
        let view = workspace().unwrap();
        assert_eq!(view.tab_count(), 7);
        assert_eq!(view.pane_count(), 9);
        assert_eq!(view.needs_you_panes(), 2);
        assert_eq!(
            view.selected_tab().map(|t| t.name.as_str()),
            Some("hero-knot")
        );
    }
}
