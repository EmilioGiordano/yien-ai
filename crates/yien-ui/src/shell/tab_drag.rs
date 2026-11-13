//! Moving a tab within its project by dragging its row, or one place at a
//! time with the keyboard (RF-ESP-16, RNF-16). The decisions are
//! `crate::tab_order`'s; this follows the drag, says where the line goes and
//! tells the core.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::*;
use yien_proto::{Command, ProjectId, TabId};

use super::Shell;
use super::bars::NoGhost;
use crate::actions::*;
use crate::model::TabView;
use crate::region::Region;
use crate::tab_order::{self, Release, Span};

/// How often a drag held near an end of the tab list scrolls it.
const SCROLL_TICK: Duration = Duration::from_millis(16);

/// What a drag of a tab row carries.
pub(super) struct TabDrag {
    tab: TabId,
    project: ProjectId,
}

/// A tab drag, from the press on a row to the drop.
#[derive(Default)]
pub(super) struct TabDragState {
    /// The row last pressed and where, until the button comes up.
    press: Option<(TabId, (f32, f32))>,
    /// The drag gpui started, if one runs.
    live: Option<Live>,
    /// The tab list's scroll, which a drag near its ends moves.
    pub(super) scroll: ScrollHandle,
}

struct Live {
    tab: TabId,
    project: ProjectId,
    press: (f32, f32),
    pointer: (f32, f32),
    /// The pointer went past the threshold: a drag, no longer a click.
    armed: bool,
    /// The rows of the tab's project, as their drag listeners last saw them.
    rows: HashMap<TabId, Span>,
    /// The gap the drop would go in.
    slot: Option<usize>,
    /// A timer is scrolling the list.
    scrolling: bool,
}

/// What a tab row draws while a tab is dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowMark {
    /// The row being dragged, dimmed as the prototype's dragged pane.
    Dragged,
    /// The tab would land above this row.
    LineAbove,
    /// The tab would land below this row, the project's last.
    LineBelow,
}

fn xy(p: Point<Pixels>) -> (f32, f32) {
    (p.x.as_f32(), p.y.as_f32())
}

fn span(b: Bounds<Pixels>) -> Span {
    Span {
        left: b.left().as_f32(),
        right: b.right().as_f32(),
        top: b.top().as_f32(),
        bottom: b.bottom().as_f32(),
    }
}

impl Shell {
    /// Lets a tab row be dragged within its project. Not while it is being
    /// renamed, where a drag selects text.
    pub(super) fn draggable_row(
        &self,
        row: Stateful<Div>,
        tab: &TabView,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (id, project) = (tab.id, tab.project);
        row.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, _| {
                this.tab_drag.press = Some((id, xy(event.position)));
            }),
        )
        .on_drag(TabDrag { tab: id, project }, |_, _, _, cx| {
            cx.new(|_| NoGhost)
        })
    }

    /// Lets a tab row report where it is while another row is dragged; a
    /// row being renamed reports too, or its project would have no gap.
    pub(super) fn drop_target_row(
        &self,
        row: Stateful<Div>,
        tab: &TabView,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (id, project) = (tab.id, tab.project);
        row.on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<TabDrag>, _, cx| {
                this.on_tab_row_drag(id, project, event, cx);
            }),
        )
    }

    /// Every row's listener sees every move of a tab drag, with its own
    /// bounds: the rows of the dragged tab's project are kept, so the gap is
    /// found among them wherever the pointer is.
    fn on_tab_row_drag(
        &mut self,
        row: TabId,
        row_project: ProjectId,
        event: &DragMoveEvent<TabDrag>,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx);
        let (tab, project) = (drag.tab, drag.project);
        let pointer = xy(event.event.position);
        let press = self
            .tab_drag
            .press
            .filter(|(t, _)| *t == tab)
            .map_or(pointer, |(_, at)| at);
        let live = match &mut self.tab_drag.live {
            Some(live) if live.tab == tab => live,
            slot => slot.insert(Live {
                tab,
                project,
                press,
                pointer,
                armed: false,
                rows: HashMap::new(),
                slot: None,
                scrolling: false,
            }),
        };
        if row_project == project {
            live.rows.insert(row, span(event.bounds));
        }
        live.pointer = pointer;
        if !live.armed && tab_order::past_threshold(live.press, pointer) {
            live.armed = true;
        }
        if self.update_drop_slot() {
            cx.notify();
        }
        self.keep_scrolling(cx);
    }

    /// Finds the gap again; whether it changed.
    fn update_drop_slot(&mut self) -> bool {
        let Some(live) = self.tab_drag.live.as_mut() else {
            return false;
        };
        let slot = if live.armed {
            self.workspace.project(live.project).and_then(|project| {
                let rows: Option<Vec<Span>> = project
                    .tabs
                    .iter()
                    .map(|t| live.rows.get(t).copied())
                    .collect();
                tab_order::drop_slot(&rows?, live.pointer, live.slot)
            })
        } else {
            None
        };
        let changed = live.slot != slot;
        live.slot = slot;
        changed
    }

    /// Where the dragged tab would end up, `None` while a drop would leave
    /// it where it is.
    fn drop_landing(&self) -> Option<(TabId, ProjectId, usize)> {
        let live = self.tab_drag.live.as_ref().filter(|l| l.armed)?;
        let (project, from, _) = self.workspace.place_of(live.tab)?;
        if project != live.project {
            return None;
        }
        let to = tab_order::landing(from, live.slot?)?;
        Some((live.tab, project, to))
    }

    /// What the row of `tab` draws for the drag under way, if anything.
    pub(super) fn row_mark(&self, tab: TabId) -> Option<RowMark> {
        let live = self.tab_drag.live.as_ref().filter(|l| l.armed)?;
        if live.tab == tab {
            return Some(RowMark::Dragged);
        }
        self.drop_landing()?;
        let slot = live.slot?;
        let tabs = &self.workspace.project(live.project)?.tabs;
        if tabs.get(slot) == Some(&tab) {
            Some(RowMark::LineAbove)
        } else if slot == tabs.len() && tabs.last() == Some(&tab) {
            Some(RowMark::LineBelow)
        } else {
            None
        }
    }

    /// The button came up after `clicks` clicks in a row. A drag past the
    /// threshold drops the tab where the line was; a shorter one was a
    /// click, which gpui no longer reports once its own drag started, so the
    /// tab is selected, or renamed on a double click, here.
    pub(super) fn end_tab_drag(
        &mut self,
        clicks: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let press = self.tab_drag.press.take();
        let landing = self.drop_landing();
        let (tab, armed) = match self.tab_drag.live.take() {
            Some(live) => (live.tab, live.armed),
            // gpui started the drag on the last move, before any row heard of it.
            None => match press {
                Some((tab, _)) if cx.has_active_drag() => (tab, false),
                _ => return,
            },
        };
        match tab_order::release(armed, clicks) {
            Release::Drop => {
                if let Some((tab, project, to)) = landing {
                    self.move_tab_to(tab, project, to, cx);
                }
            }
            Release::Select => self.select_tab(tab, window, cx),
            Release::Rename => self.start_rename(tab, window, cx),
        }
        cx.notify();
    }

    /// Escape, or the window losing the focus: the drag ends where it
    /// started. Whether one was under way.
    pub(super) fn cancel_tab_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let pending = self.tab_drag.press.take().is_some() && cx.has_active_drag();
        if self.tab_drag.live.take().is_none() && !pending {
            return false;
        }
        cx.stop_active_drag(window);
        cx.notify();
        true
    }

    /// Drops what is left of a drag gpui ended without a button release
    /// reaching the window.
    pub(super) fn forget_ended_tab_drag(&mut self, cx: &App) {
        if !cx.has_active_drag() {
            self.tab_drag.live = None;
        }
    }

    fn move_tab_to(&mut self, tab: TabId, project: ProjectId, to: usize, cx: &mut Context<Self>) {
        if self.workspace.move_tab(tab, to) {
            let to_index = u32::try_from(to).unwrap_or(u32::MAX);
            self.command(
                Command::MoveTab {
                    tab,
                    project,
                    to_index,
                },
                cx,
            );
            cx.notify();
        }
    }

    /// The tab a tab command acts on: the one under the cursor while the
    /// tab bar has the keyboard, else the selected one.
    pub(super) fn keyboard_tab(&self, window: &Window, cx: &App) -> Option<TabId> {
        let in_bar = self.region(window, cx) == Region::TabBar;
        let selected = self.workspace.selected_tab().map(|t| t.id);
        self.tab_cursor.filter(|_| in_bar).or(selected)
    }

    /// One place up or down: [`Shell::keyboard_tab`]. Nothing at either end.
    fn move_tab_by(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.keyboard_tab(window, cx) else {
            return;
        };
        let Some((project, from, len)) = self.workspace.place_of(tab) else {
            return;
        };
        if let Some(to) = tab_order::step(from, len, down) {
            self.move_tab_to(tab, project, to, cx);
        }
    }

    pub(super) fn move_tab_up(
        &mut self,
        _: &MoveTabUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_tab_by(false, window, cx);
    }

    pub(super) fn move_tab_down(
        &mut self,
        _: &MoveTabDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_tab_by(true, window, cx);
    }

    /// Starts scrolling the tab list while the pointer is near one of its
    /// ends; mouse moves alone would leave it still under a pointer held
    /// there.
    fn keep_scrolling(&mut self, cx: &mut Context<Self>) {
        let bounds = self.tab_drag.scroll.bounds();
        let Some(live) = self.tab_drag.live.as_mut().filter(|l| l.armed) else {
            return;
        };
        let by = tab_order::scroll_step(live.pointer, span(bounds));
        if live.scrolling || by == 0.0 {
            return;
        }
        live.scrolling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SCROLL_TICK).await;
                let going = this.update(cx, |shell, cx| shell.scroll_tick(cx));
                if !matches!(going, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    /// One step of the scroll; whether to go on.
    fn scroll_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let scroll = self.tab_drag.scroll.clone();
        let Some(live) = self.tab_drag.live.as_mut() else {
            return false;
        };
        let bounds = scroll.bounds();
        let by = tab_order::scroll_step(live.pointer, span(bounds));
        let offset = scroll.offset();
        let y = tab_order::scrolled(offset.y.as_f32(), by, scroll.max_offset().y.as_f32());
        let moved = y - offset.y.as_f32();
        if moved == 0.0 {
            live.scrolling = false;
            return false;
        }
        scroll.set_offset(point(offset.x, px(y)));
        // The rows moved with the list; the next mouse move measures them again.
        for row in live.rows.values_mut() {
            *row = row.shifted(moved);
        }
        self.update_drop_slot();
        cx.notify();
        true
    }
}
