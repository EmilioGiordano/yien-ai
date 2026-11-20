//! The new tab menu (RF-ESP-03, RF-PAN-18): the chevron beside a project
//! head's "+", a right click on the "+", "New tab with…" in the tab menu or
//! `tab: new tab with…` list the installed shells, the default first, and
//! the agent profiles, and open a tab in that project running the one picked.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::*;
use yien_proto::{Command, ProfileId, ProfileInfo, ProjectId, SHELL_PROFILE, is_shell_profile};

use super::menu::{
    EDGE_MARGIN, Menu, MenuEntry, MenuEvent, MenuItem, MenuOutcome, OpenMenu, closed_by_this_click,
};
use super::pane_menu::menu_may_open;
use super::tab_menu::menu_returns_to_bar;
use super::{CLAUDE_PROFILE, Shell};
use crate::actions::NewTabMenu;
use crate::catalog::{shortcut, spec};

/// The catalog command that opens the same tab as an item, by profile id.
fn keyed_command(profile: &str) -> Option<&'static str> {
    match profile {
        SHELL_PROFILE => Some("tab.new"),
        CLAUDE_PROFILE => Some("tab.new_claude"),
        _ => None,
    }
}

/// The menu for `profiles` in the core's order: the shells, the default
/// one first and marked, a separator, then the agents. `keys` answers with
/// a command's current keys.
pub(super) fn new_tab_menu(
    profiles: &[ProfileInfo],
    keys: &dyn Fn(&str) -> Option<String>,
) -> Menu<ProfileId> {
    let item = |p: &ProfileInfo| {
        let label = if p.id.as_str() == SHELL_PROFILE {
            format!("{} (default)", p.name)
        } else {
            p.name.clone()
        };
        MenuEntry::Item(MenuItem {
            label,
            keys: keyed_command(p.id.as_str()).and_then(keys),
            enabled: true,
            action: p.id.clone(),
        })
    };
    let (mut shells, agents): (Vec<&ProfileInfo>, Vec<&ProfileInfo>) = profiles
        .iter()
        .partition(|p| is_shell_profile(p.id.as_str()));
    shells.sort_by_key(|p| p.id.as_str() != SHELL_PROFILE);
    let mut entries: Vec<MenuEntry<ProfileId>> = shells.into_iter().map(item).collect();
    if !entries.is_empty() && !agents.is_empty() {
        entries.push(MenuEntry::Separator);
    }
    entries.extend(agents.into_iter().map(item));
    Menu::new(entries)
}

/// Whether the split menu draws a separator before `profiles[at]`: the
/// first agent after the shells.
pub(super) fn agents_start_at(profiles: &[ProfileInfo], at: usize) -> bool {
    let shell = |i: usize| {
        profiles
            .get(i)
            .is_some_and(|p| is_shell_profile(p.id.as_str()))
    };
    at > 0 && at < profiles.len() && shell(at - 1) && !shell(at)
}

/// The command a pick sends: the default shell as the "+" opens it, any
/// other profile by its id.
pub(super) fn new_tab_command(project: ProjectId, profile: &ProfileId) -> Command {
    Command::NewTab {
        project,
        name: None,
        profile: (profile.as_str() != SHELL_PROFILE).then(|| profile.clone()),
    }
}

/// The new tab menu while open, and where each project's chevron was
/// painted.
#[derive(Default)]
pub(super) struct NewTabMenuState {
    open: Option<OpenNewTabMenu>,
    buttons: Rc<RefCell<HashMap<ProjectId, Bounds<Pixels>>>>,
    /// The menu a mouse-down outside closed last, so the click on its own
    /// chevron that did it does not open it again.
    closed: Option<(ProjectId, Instant)>,
}

struct OpenNewTabMenu {
    project: ProjectId,
    back_to_bar: bool,
    menu: OpenMenu<ProfileId>,
}

impl NewTabMenuState {
    pub(super) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Drops the menu without moving the keyboard, for a menu opening now.
    pub(super) fn forget(&mut self) {
        self.open = None;
    }

    /// Where a menu opened by keyboard goes for `project`: under its
    /// chevron, else the window's corner.
    fn anchor(&self, project: ProjectId) -> Point<Pixels> {
        self.buttons.borrow().get(&project).map_or_else(
            || point(px(EDGE_MARGIN), px(EDGE_MARGIN)),
            |b| point(b.left(), b.bottom() + px(2.0)),
        )
    }
}

impl Shell {
    /// The chevron beside a project head's "+": opens the new tab menu.
    pub(super) fn new_tab_menu_button(
        &self,
        project: ProjectId,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = self.theme;
        let hover = t.hover;
        let buttons = Rc::clone(&self.new_tab_menu.buttons);
        div()
            .id(("new-tab-menu", project.get()))
            .role(Role::Button)
            .aria_label(format!("New tab in {name} with a shell or an agent"))
            .aria_expanded(
                self.new_tab_menu
                    .open
                    .as_ref()
                    .is_some_and(|o| o.project == project),
            )
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(16.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                let closed = this.new_tab_menu.closed.take();
                if closed_by_this_click(closed.as_ref(), &project, Instant::now()) {
                    return;
                }
                let at = this.new_tab_menu.anchor(project);
                this.open_new_tab_menu(project, at, window, cx);
            }))
            .child(
                svg()
                    .path(crate::assets::CHEVRON)
                    .size(px(10.0))
                    .text_color(t.faint),
            )
            .child(
                canvas(
                    move |bounds, _, _| {
                        buttons.borrow_mut().insert(project, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }

    /// Opens the new tab menu of `project` with its top left at `at`,
    /// moving the keyboard to it.
    pub(super) fn open_new_tab_menu(
        &mut self,
        project: ProjectId,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !menu_may_open(
            self.confirm.is_some(),
            self.worktree_close.is_some(),
            self.worktree_prompt.is_some(),
        ) {
            return;
        }
        let Some(name) = self.workspace.project(project).map(|p| p.name.clone()) else {
            return;
        };
        let back_to_bar = menu_returns_to_bar(self.keyboard_in_bar, self.region(window, cx));
        // Dropped first, so their blur cannot close the menu opened now.
        self.new_tab_menu.open = None;
        self.tab_menu.forget();
        self.file_menu.forget();
        self.menu = None;
        self.split_menu = None;
        self.close_tray(window, cx);
        self.close_palette(window, cx);
        self.finish_rename(true, window, cx);
        let model = {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let keys = |id: &str| shortcut(&keymap, (spec(id)?.action)().as_ref());
            new_tab_menu(self.workspace.profiles(), &keys)
        };
        let menu = OpenMenu::open(
            model,
            format!("New tab in {name}"),
            at,
            window,
            cx,
            |this: &mut Self, cx| {
                this.new_tab_menu.open = None;
                cx.notify();
            },
        );
        self.new_tab_menu.open = Some(OpenNewTabMenu {
            project,
            back_to_bar,
            menu,
        });
        cx.notify();
    }

    /// `tab: new tab with…`: the menu of the project a new tab goes to,
    /// under its chevron.
    pub(super) fn new_tab_menu_command(
        &mut self,
        _: &NewTabMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.workspace.project_for_new_tab() {
            Some(project) => {
                let at = self.new_tab_menu.anchor(project);
                self.open_new_tab_menu(project, at, window, cx);
            }
            None => self.say("There is no project to open a tab in.".to_owned(), cx),
        }
    }

    fn close_new_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.new_tab_menu.open.take() else {
            return;
        };
        if open.back_to_bar {
            window.focus(&self.tab_bar_focus, cx);
            self.keyboard_in_bar = true;
            cx.notify();
        } else {
            self.give_back_keyboard(window, cx);
        }
    }

    fn on_new_tab_menu_event(
        &mut self,
        event: MenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.new_tab_menu.open.as_mut() else {
            return;
        };
        let project = open.project;
        if event == MenuEvent::ClickedOutside {
            self.new_tab_menu.closed = Some((project, Instant::now()));
        }
        let Some(open) = self.new_tab_menu.open.as_mut() else {
            return;
        };
        match open.menu.model_mut().respond(event) {
            MenuOutcome::Stay => cx.notify(),
            MenuOutcome::Close => self.close_new_tab_menu(window, cx),
            MenuOutcome::Run(profile) => {
                self.close_new_tab_menu(window, cx);
                if self.workspace.is_folded(project) {
                    self.toggle_fold(project, cx);
                }
                self.command(new_tab_command(project, &profile), cx);
            }
        }
    }

    pub(super) fn render_new_tab_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.new_tab_menu.open.as_ref()?;
        Some(
            open.menu
                .render(&self.theme, cx, Self::on_new_tab_menu_event),
        )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the gpui glob there would shadow `#[test]`.
    use super::{
        Menu, MenuEntry, MenuEvent, MenuOutcome, agents_start_at, new_tab_command, new_tab_menu,
    };
    use crate::catalog::default_keys;
    use yien_proto::{Command, ProfileId, ProfileInfo, ProjectId, ShellKind};

    fn info(id: ProfileId, name: &str) -> ProfileInfo {
        ProfileInfo {
            id,
            name: name.into(),
        }
    }

    /// The core's list: the default shell, the installed shells, the agents.
    fn profiles() -> Vec<ProfileInfo> {
        let mut list = vec![info(ProfileId::new("shell"), "Shell")];
        for kind in [ShellKind::Cmd, ShellKind::GitBash] {
            list.push(info(kind.profile(), kind.name()));
        }
        list.push(info(ProfileId::new("claude"), "Claude Code"));
        list.push(info(ProfileId::new("codex"), "Codex"));
        list
    }

    fn labels(menu: &Menu<ProfileId>) -> Vec<String> {
        menu.entries()
            .iter()
            .map(|e| match e {
                MenuEntry::Item(i) => i.label.clone(),
                MenuEntry::Separator => "--".to_owned(),
            })
            .collect()
    }

    #[test]
    fn rf_esp_03_the_menu_lists_the_default_shell_first_then_shells_then_agents() {
        let menu = new_tab_menu(&profiles(), &|_| None);
        assert_eq!(
            labels(&menu),
            [
                "Shell (default)",
                "Command Prompt",
                "Git Bash",
                "--",
                "Claude Code",
                "Codex"
            ]
        );
        assert_eq!(menu.chosen(), Some(&ProfileId::new("shell")));
        // The default leads even when the list does not start with it.
        let mut shuffled = profiles();
        shuffled.rotate_left(1);
        assert_eq!(
            labels(&new_tab_menu(&shuffled, &|_| None))[0],
            "Shell (default)"
        );
    }

    #[test]
    fn rf_esp_03_no_separator_without_agents_or_shells() {
        let shells: Vec<ProfileInfo> = profiles().into_iter().take(3).collect();
        assert!(!labels(&new_tab_menu(&shells, &|_| None)).contains(&"--".to_owned()));
        let agents: Vec<ProfileInfo> = profiles().into_iter().skip(3).collect();
        assert_eq!(
            labels(&new_tab_menu(&agents, &|_| None)),
            ["Claude Code", "Codex"]
        );
        assert!(new_tab_menu(&[], &|_| None).entries().is_empty());
    }

    #[test]
    fn rf_nav_03_keyed_items_show_their_keys() {
        let menu = new_tab_menu(&profiles(), &default_keys);
        let keys: Vec<Option<String>> = menu
            .entries()
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item(i) => Some(i.keys.clone()),
                MenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(keys[0], default_keys("tab.new"));
        assert_eq!(keys[3], default_keys("tab.new_claude"));
        assert!(keys[0].is_some() && keys[3].is_some());
        assert_eq!(
            (keys[1].clone(), keys[2].clone(), keys[4].clone()),
            (None, None, None)
        );
    }

    #[test]
    fn rf_pan_18_the_split_menu_separates_the_shells_from_the_agents() {
        let list = profiles();
        let breaks: Vec<usize> = (0..=list.len())
            .filter(|at| agents_start_at(&list, *at))
            .collect();
        assert_eq!(breaks, [3], "before Claude Code");
        let agents: Vec<ProfileInfo> = list.into_iter().skip(3).collect();
        assert!((0..agents.len()).all(|at| !agents_start_at(&agents, at)));
    }

    #[test]
    fn rf_esp_03_rf_pan_18_picking_an_item_opens_a_tab_running_it() {
        let project = ProjectId::new(4);
        let mut menu = new_tab_menu(&profiles(), &|_| None);
        let mut run = |event: MenuEvent| match menu.respond(event) {
            MenuOutcome::Run(profile) => new_tab_command(project, &profile),
            other => panic!("{other:?}"),
        };
        let tab = |profile: Option<ProfileId>| Command::NewTab {
            project,
            name: None,
            profile,
        };
        assert_eq!(run(MenuEvent::Confirm), tab(None), "the default shell");
        assert_eq!(
            run(MenuEvent::Pick(2)),
            tab(Some(ShellKind::GitBash.profile()))
        );
        assert_eq!(run(MenuEvent::Pick(4)), tab(Some(ProfileId::new("claude"))));
        assert_eq!(
            menu.respond(MenuEvent::Pick(3)),
            MenuOutcome::Stay,
            "the separator"
        );
    }
}
