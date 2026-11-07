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

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(ids: &[u64]) -> Vec<TabId> {
        ids.iter().map(|id| TabId::new(*id)).collect()
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
