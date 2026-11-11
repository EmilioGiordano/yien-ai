//! The three regions the keyboard moves between (RF-NAV-08): the tab bar,
//! the pane area and the context bar, and the tab bar's own cursor. Decided
//! here without drawing; the shell moves the gpui focus.

use yien_proto::TabId;

/// A part of the window that can hold the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Region {
    TabBar,
    Panes,
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
/// next region left to right, wrapping, past the bars that are hidden. The
/// pane area is always there; a hidden region counts as the pane area.
pub(crate) fn next_region(from: Region, forward: bool, tab_bar: bool, context_bar: bool) -> Region {
    let order: Vec<Region> = [
        (Region::TabBar, tab_bar),
        (Region::Panes, true),
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
        let tab_bar = next_region(Panes, false, true, true);
        assert!(ring_on(TabBar, tab_bar, Modality::Keyboard));
        assert!(!ring_on(ContextBar, tab_bar, Modality::Keyboard));
        let context_bar = next_region(Panes, true, true, true);
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
        let at = next_region(Panes, true, true, true);
        assert!(ring_on(ContextBar, at, Modality::Keyboard));
        // A click on a file row keeps the keyboard in the bar: no ring.
        assert!(!ring_on(ContextBar, at, Modality::Pointer));
        // Shift+F6 to the panes, F6 back: the ring returns.
        let at = next_region(next_region(at, false, true, true), true, true, true);
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
            at = next_region(at, true, true, true);
            seen.push(at);
        }
        assert_eq!(seen, [ContextBar, TabBar, Panes]);
        assert_eq!(next_region(Panes, false, true, true), TabBar);
        assert_eq!(next_region(TabBar, false, true, true), ContextBar);
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
        assert_eq!(next_region(Panes, true, false, true), ContextBar);
        assert_eq!(next_region(ContextBar, true, false, true), Panes);
        assert_eq!(next_region(Panes, true, true, false), TabBar);
        assert_eq!(next_region(Panes, true, false, false), Panes);
        assert_eq!(next_region(Panes, false, false, false), Panes);
        // The keyboard sat in a bar that has just been hidden.
        assert_eq!(next_region(TabBar, true, false, true), ContextBar);
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
