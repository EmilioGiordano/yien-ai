//! The regions the keyboard moves between (RF-NAV-08): the tab bar, the
//! pane area, the pane dividers and the context bar, and the cursors of the
//! tab bar and the dividers. Decided here without drawing; the shell moves
//! the gpui focus.

use yien_proto::{PaneId, TabId};

/// A part of the window that can hold the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Region {
    TabBar,
    Panes,
    /// The dividers between the selected tab's panes (RF-PAN-02).
    Dividers,
    ContextBar,
}

/// How the user last acted: a key that moved the keyboard, or a mouse
/// button. Only the focus rings depend on it, never where the keyboard is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Modality {
    Keyboard,
    Pointer,
}

/// Whether `bar` draws its keyboard ring (the bar border and its cursor
/// row) while `holder` has the keyboard: only when the keyboard got there
/// by keyboard, never after a click. The pane area has no bar ring.
pub(crate) fn ring_on(bar: Region, holder: Region, modality: Modality) -> bool {
    bar != Region::Panes && bar == holder && modality == Modality::Keyboard
}

/// Where F6 (`forward`) or Shift+F6 moves the keyboard from `from`: the
/// next region left to right, wrapping, past the bars that are hidden and
/// past the dividers when the tab shows none. The pane area is always
/// there; a hidden region counts as the pane area.
pub(crate) fn next_region(
    from: Region,
    forward: bool,
    tab_bar: bool,
    context_bar: bool,
    dividers: bool,
) -> Region {
    let order: Vec<Region> = [
        (Region::TabBar, tab_bar),
        (Region::Panes, true),
        (Region::Dividers, dividers),
        (Region::ContextBar, context_bar),
    ]
    .into_iter()
    .filter_map(|(region, shown)| shown.then_some(region))
    .collect();
    let at = order
        .iter()
        .position(|r| *r == from)
        .or_else(|| order.iter().position(|r| *r == Region::Panes))
        .unwrap_or(0);
    let len = order.len();
    let next = if forward {
        (at + 1) % len
    } else {
        (at + len - 1) % len
    };
    order[next]
}

/// The row the tab bar's cursor lands on after moving `by` rows from `at`
/// among the rows drawn (`rows`, top to bottom), stopping at either end. A
/// cursor that is not on a drawn row starts from `selected`, else the top.
pub(crate) fn move_cursor(
    rows: &[TabId],
    at: Option<TabId>,
    selected: Option<TabId>,
    by: isize,
) -> Option<TabId> {
    let index = |id: Option<TabId>| id.and_then(|id| rows.iter().position(|r| *r == id));
    let Some(from) = index(at).or_else(|| index(selected)) else {
        return rows.first().copied();
    };
    let last = rows.len().saturating_sub(1);
    let to = from.saturating_add_signed(by).min(last);
    rows.get(to).copied()
}

/// Where the cursor starts when the tab bar takes the keyboard: where it
/// was if that row is still drawn, else on the selected tab, else the top.
pub(crate) fn cursor_on_entry(
    rows: &[TabId],
    at: Option<TabId>,
    selected: Option<TabId>,
) -> Option<TabId> {
    move_cursor(rows, at, selected, 0)
}

/// A pane divider by the first pane on each of its sides.
pub(crate) type DividerKey = (PaneId, PaneId);

/// The divider Tab (`forward`) or Shift+Tab moves to among those drawn
/// (`all`, in reading order), wrapping. One that is not drawn any more
/// starts over at the first, or the last going back.
pub(crate) fn next_divider(
    all: &[DividerKey],
    at: Option<DividerKey>,
    forward: bool,
) -> Option<DividerKey> {
    let len = all.len();
    let next = match at.and_then(|at| all.iter().position(|d| *d == at)) {
        Some(i) if forward => (i + 1) % len,
        Some(i) => (i + len - 1) % len,
        None if forward => 0,
        None => len.checked_sub(1)?,
    };
    all.get(next).copied()
}

/// Where the divider cursor lands when the dividers take the keyboard: where
/// it was if that divider is still drawn, else the first coming forward
/// (F6) or the last coming back (Shift+F6).
pub(crate) fn divider_on_entry(
    all: &[DividerKey],
    at: Option<DividerKey>,
    forward: bool,
) -> Option<DividerKey> {
    match at.filter(|at| all.contains(at)) {
        Some(at) => Some(at),
        None => next_divider(all, None, forward),
    }
}

/// Whether F6's claim on a bar still holds once the keyboard is seen in
/// `now`. The claim lasts while the keyboard stays in the bars; once it is
/// in the pane area (a click on a terminal), a later click in a bar is only
/// a click, and the pane gets the keyboard after it.
pub(crate) fn bar_claim_holds(claimed: bool, now: Region) -> bool {
    claimed && now != Region::Panes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(ids: &[u64]) -> Vec<TabId> {
        ids.iter().map(|id| TabId::new(*id)).collect()
    }

    #[test]
    fn rf_nav_08_the_ring_shows_after_a_keyboard_move_into_a_bar() {
        use Region::*;
        let tab_bar = next_region(Panes, false, true, true, false);
        assert!(ring_on(TabBar, tab_bar, Modality::Keyboard));
        assert!(!ring_on(ContextBar, tab_bar, Modality::Keyboard));
        let context_bar = next_region(Panes, true, true, true, false);
        assert!(ring_on(ContextBar, context_bar, Modality::Keyboard));
        assert!(!ring_on(TabBar, context_bar, Modality::Keyboard));
    }

    #[test]
    fn rf_nav_08_a_click_in_a_bar_shows_no_ring() {
        use Region::*;
        assert!(!ring_on(TabBar, TabBar, Modality::Pointer));
        assert!(!ring_on(ContextBar, ContextBar, Modality::Pointer));
    }

    #[test]
    fn rf_nav_08_a_click_after_f6_hides_the_ring_and_f6_shows_it_again() {
        use Region::*;
        // F6 from the panes into the context bar.
        let at = next_region(Panes, true, true, true, false);
        assert!(ring_on(ContextBar, at, Modality::Keyboard));
        // A click on a file row keeps the keyboard in the bar: no ring.
        assert!(!ring_on(ContextBar, at, Modality::Pointer));
        // Shift+F6 to the panes, F6 back: the ring returns.
        let at = next_region(
            next_region(at, false, true, true, false),
            true,
            true,
            true,
            false,
        );
        assert!(ring_on(ContextBar, at, Modality::Keyboard));
    }

    #[test]
    fn rf_nav_08_the_pane_area_never_shows_a_bar_ring() {
        use Region::*;
        for modality in [Modality::Keyboard, Modality::Pointer] {
            assert!(!ring_on(Panes, Panes, modality));
            assert!(!ring_on(TabBar, Panes, modality));
            assert!(!ring_on(ContextBar, Panes, modality));
        }
    }

    #[test]
    fn rf_nav_08_f6_cycles_tab_bar_panes_context_bar_and_back() {
        use Region::*;
        let mut at = Panes;
        let mut seen = Vec::new();
        for _ in 0..3 {
            at = next_region(at, true, true, true, false);
            seen.push(at);
        }
        assert_eq!(seen, [ContextBar, TabBar, Panes]);
        assert_eq!(next_region(Panes, false, true, true, false), TabBar);
        assert_eq!(next_region(TabBar, false, true, true, false), ContextBar);
    }

    #[test]
    fn rf_nav_08_the_bar_claim_ends_once_the_keyboard_is_in_the_panes() {
        use Region::*;
        // F6 into the tab bar, then on to the context bar: still claimed.
        assert!(bar_claim_holds(true, TabBar));
        assert!(bar_claim_holds(true, ContextBar));
        // A click on a terminal, then on another tab row: the claim is gone
        // and stays gone, so the tab row does not keep the keyboard.
        let after_click = bar_claim_holds(true, Panes);
        assert!(!after_click);
        assert!(!bar_claim_holds(after_click, TabBar));
    }

    #[test]
    fn rf_nav_08_hidden_bars_are_skipped() {
        use Region::*;
        assert_eq!(next_region(Panes, true, false, true, false), ContextBar);
        assert_eq!(next_region(ContextBar, true, false, true, false), Panes);
        assert_eq!(next_region(Panes, true, true, false, false), TabBar);
        assert_eq!(next_region(Panes, true, false, false, false), Panes);
        assert_eq!(next_region(Panes, false, false, false, false), Panes);
        // The keyboard sat in a bar that has just been hidden.
        assert_eq!(next_region(TabBar, true, false, true, false), ContextBar);
    }

    #[test]
    fn rf_pan_02_f6_reaches_the_dividers_between_the_panes_and_the_context_bar() {
        use Region::*;
        let mut at = Panes;
        let mut seen = Vec::new();
        for _ in 0..4 {
            at = next_region(at, true, true, true, true);
            seen.push(at);
        }
        assert_eq!(seen, [Dividers, ContextBar, TabBar, Panes]);
        assert_eq!(next_region(ContextBar, false, true, true, true), Dividers);
        assert_eq!(next_region(Dividers, false, true, true, true), Panes);
        // A tab with one pane has no divider to stop at.
        assert_eq!(next_region(Panes, true, true, true, false), ContextBar);
        // The dividers went away while they had the keyboard.
        assert_eq!(next_region(Dividers, true, true, true, false), ContextBar);
        assert!(ring_on(Dividers, Dividers, Modality::Keyboard));
        assert!(!ring_on(Dividers, Dividers, Modality::Pointer));
        assert!(bar_claim_holds(true, Dividers));
    }

    #[test]
    fn rf_pan_02_tab_cycles_the_dividers_and_entry_keeps_the_last_one() {
        let key = |a, b| (PaneId::new(a), PaneId::new(b));
        let all = [key(1, 2), key(2, 3), key(1, 4)];
        let gone = key(7, 8);
        assert_eq!(next_divider(&all, Some(all[0]), true), Some(all[1]));
        assert_eq!(next_divider(&all, Some(all[2]), true), Some(all[0]));
        assert_eq!(next_divider(&all, Some(all[0]), false), Some(all[2]));
        assert_eq!(next_divider(&all, Some(gone), true), Some(all[0]));
        assert_eq!(next_divider(&all, None, false), Some(all[2]));
        assert_eq!(next_divider(&[], None, true), None);
        assert_eq!(next_divider(&[], None, false), None);
        assert_eq!(divider_on_entry(&all, Some(all[1]), true), Some(all[1]));
        assert_eq!(divider_on_entry(&all, Some(gone), true), Some(all[0]));
        assert_eq!(divider_on_entry(&all, None, false), Some(all[2]));
    }

    #[test]
    fn rf_nav_08_the_tab_cursor_moves_over_drawn_rows_and_stops_at_the_ends() {
        let rows = tabs(&[10, 11, 20]);
        let t = |id| Some(TabId::new(id));
        assert_eq!(move_cursor(&rows, t(10), t(10), 1), t(11));
        assert_eq!(move_cursor(&rows, t(11), t(10), 1), t(20));
        assert_eq!(move_cursor(&rows, t(20), t(10), 1), t(20));
        assert_eq!(move_cursor(&rows, t(11), t(10), -1), t(10));
        assert_eq!(move_cursor(&rows, t(10), t(10), -1), t(10));
    }

    #[test]
    fn rf_nav_08_the_cursor_starts_on_the_selected_tab() {
        let rows = tabs(&[10, 11, 20]);
        let t = |id| Some(TabId::new(id));
        assert_eq!(cursor_on_entry(&rows, None, t(11)), t(11));
        assert_eq!(cursor_on_entry(&rows, t(20), t(11)), t(20));
        // A row that folded away or closed.
        assert_eq!(cursor_on_entry(&rows, t(99), t(11)), t(11));
        assert_eq!(cursor_on_entry(&rows, None, None), t(10));
        assert_eq!(cursor_on_entry(&[], t(10), t(10)), None);
        assert_eq!(move_cursor(&rows, t(99), None, 1), t(10));
    }
}
