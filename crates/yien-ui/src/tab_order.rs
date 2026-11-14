//! Moving a tab within its project (RF-ESP-16): when a press on a tab row
//! turns into a drag, where a drop lands, how the list scrolls under a drag
//! and the keyboard's one-step move. Decided here without drawing; the shell
//! draws.

/// How far the pointer goes from the press before a drag starts, as the
/// prototype's pane drag. A shorter move is still a click.
pub(crate) const DRAG_THRESHOLD: f32 = 6.0;
/// How far past the middle of a row the pointer must go before the drop
/// leaves the place it shows, so a pointer resting on a middle does not
/// make the line flicker.
pub(crate) const SLOT_HYSTERESIS: f32 = 4.0;
/// The band at each end of the tab list where a drag scrolls it.
pub(crate) const SCROLL_EDGE: f32 = 28.0;
/// The most a drag scrolls the list per step, at the very end of the band.
pub(crate) const SCROLL_STEP: f32 = 14.0;

/// A tab row's edges in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Span {
    pub(crate) left: f32,
    pub(crate) right: f32,
    pub(crate) top: f32,
    pub(crate) bottom: f32,
}

impl Span {
    fn middle(self) -> f32 {
        (self.top + self.bottom) / 2.0
    }

    fn half(self) -> f32 {
        (self.bottom - self.top).max(0.0) / 2.0
    }

    /// The same row moved down by `dy`.
    pub(crate) fn shifted(self, dy: f32) -> Span {
        Span {
            top: self.top + dy,
            bottom: self.bottom + dy,
            ..self
        }
    }
}

/// Whether the pointer went far enough from the press for a drag.
pub(crate) fn past_threshold(press: (f32, f32), now: (f32, f32)) -> bool {
    (now.0 - press.0).hypot(now.1 - press.1) >= DRAG_THRESHOLD
}

/// What the button coming up on a pressed tab row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Release {
    /// The press became a drag: the tab goes where the line is.
    Drop,
    Select,
    /// The second click of a double click.
    Rename,
}

/// The release of a press that went [`past_threshold`] (`armed`) or not,
/// on the `clicks`-th click in a row. gpui starts its own drag after 2 px,
/// which swallows the click, so a double click that shook a little lands
/// here and must still rename.
pub(crate) fn release(armed: bool, clicks: usize) -> Release {
    if armed {
        Release::Drop
    } else if clicks >= 2 {
        Release::Rename
    } else {
        Release::Select
    }
}

/// Where a tab dropped at `(x, y)` goes among `rows`, a project's rows top
/// to bottom: a gap from 0 (above the first) to `rows.len()` (below the
/// last). `None` when the pointer is beside the rows, or farther than half a
/// row above or below them, so a drop over another project or the panes
/// goes nowhere. `held` is the gap shown now; it stays until the pointer is
/// [`SLOT_HYSTERESIS`] past the middle that bounds it.
pub(crate) fn drop_slot(rows: &[Span], (x, y): (f32, f32), held: Option<usize>) -> Option<usize> {
    let (first, last) = (rows.first()?, rows.last()?);
    if !(x.is_finite() && y.is_finite())
        || x < first.left
        || x > first.right
        || y < first.top - first.half()
        || y > last.bottom + last.half()
    {
        return None;
    }
    let slot = rows.iter().take_while(|r| r.middle() <= y).count();
    let Some(held) = held.filter(|&h| h <= rows.len() && h != slot) else {
        return Some(slot);
    };
    let above = held
        .checked_sub(1)
        .and_then(|i| rows.get(i))
        .map_or(f32::NEG_INFINITY, |r| r.middle());
    let below = rows.get(held).map_or(f32::INFINITY, |r| r.middle());
    let keeps = y >= above - SLOT_HYSTERESIS && y <= below + SLOT_HYSTERESIS;
    Some(if keeps { held } else { slot })
}

/// The place a tab at `from` ends at when dropped in gap `slot`, counted
/// once it has left its old place. `None` when it would stay where it is:
/// the gaps just above and just below it.
pub(crate) fn landing(from: usize, slot: usize) -> Option<usize> {
    if slot == from || slot == from + 1 {
        None
    } else if slot > from {
        Some(slot - 1)
    } else {
        Some(slot)
    }
}

/// The place one step up or down from `from` in a project of `len` tabs;
/// `None` at either end, where the keyboard move does nothing.
pub(crate) fn step(from: usize, len: usize, down: bool) -> Option<usize> {
    if down {
        from.checked_add(1).filter(|&to| to < len)
    } else {
        from.checked_sub(1).filter(|_| from < len)
    }
}

/// How much a drag at `(x, y)` scrolls `list`, the tab list as drawn:
/// positive towards its start, negative towards its end, faster deeper into
/// the band at each end, nothing in between. Nothing either while the
/// pointer is beside the list, over the panes.
pub(crate) fn scroll_step((x, y): (f32, f32), list: Span) -> f32 {
    let Span {
        left,
        right,
        top,
        bottom,
    } = list;
    if !(x.is_finite() && y.is_finite() && top.is_finite() && bottom.is_finite())
        || bottom <= top
        || x < left
        || x > right
    {
        return 0.0;
    }
    let speed = |depth: f32| SCROLL_STEP * (depth / SCROLL_EDGE).clamp(0.0, 1.0);
    let (from_top, from_bottom) = (y - top, bottom - y);
    if from_top < SCROLL_EDGE && from_top <= from_bottom {
        speed(SCROLL_EDGE - from_top)
    } else if from_bottom < SCROLL_EDGE {
        -speed(SCROLL_EDGE - from_bottom)
    } else {
        0.0
    }
}

/// A list's scroll offset moved by `by`, kept between its end (`-max`) and
/// its start (0), as gpui counts it.
pub(crate) fn scrolled(offset: f32, by: f32, max: f32) -> f32 {
    (offset + by).clamp(-max.max(0.0), 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows 30 px tall from y = 100, as the tab bar draws them.
    fn rows(n: usize) -> Vec<Span> {
        (0..n)
            .map(|i| {
                let top = 100.0 + 30.0 * i as f32;
                Span {
                    left: 10.0,
                    right: 250.0,
                    top,
                    bottom: top + 30.0,
                }
            })
            .collect()
    }

    #[test]
    fn rf_esp_16_a_short_move_stays_a_click() {
        assert!(!past_threshold((10.0, 10.0), (10.0, 10.0)));
        assert!(
            !past_threshold((10.0, 10.0), (13.0, 14.0)),
            "5 px is a click"
        );
        assert!(!past_threshold((10.0, 10.0), (10.0, 15.9)));
        assert!(past_threshold((10.0, 10.0), (10.0, 16.0)));
        assert!(past_threshold((10.0, 10.0), (4.0, 10.0)), "any direction");
    }

    #[test]
    fn rf_esp_16_a_double_click_that_shook_into_a_drag_still_renames() {
        assert_eq!(release(false, 1), Release::Select);
        assert_eq!(release(false, 2), Release::Rename);
        assert_eq!(release(false, 3), Release::Rename, "a triple click");
        assert_eq!(release(true, 1), Release::Drop);
        assert_eq!(release(true, 2), Release::Drop, "a real drag moves");
        assert_eq!(release(false, 0), Release::Select);
    }

    #[test]
    fn rf_esp_16_the_drop_gap_follows_the_middles_of_the_rows() {
        let rows = rows(3);
        assert_eq!(drop_slot(&rows, (100.0, 100.0), None), Some(0));
        assert_eq!(drop_slot(&rows, (100.0, 114.9), None), Some(0));
        assert_eq!(drop_slot(&rows, (100.0, 115.0), None), Some(1));
        assert_eq!(drop_slot(&rows, (100.0, 150.0), None), Some(2));
        assert_eq!(drop_slot(&rows, (100.0, 189.0), None), Some(3));
        // Half a row past either end still drops there.
        assert_eq!(drop_slot(&rows, (100.0, 86.0), None), Some(0));
        assert_eq!(drop_slot(&rows, (100.0, 204.0), None), Some(3));
    }

    #[test]
    fn rf_esp_16_far_from_the_projects_rows_there_is_no_drop() {
        let rows = rows(3);
        assert_eq!(drop_slot(&rows, (100.0, 84.0), None), None);
        assert_eq!(drop_slot(&rows, (100.0, 206.0), None), None);
        assert_eq!(drop_slot(&rows, (100.0, f32::NAN), None), None);
        // Beside the rows, over the panes, there is no drop either.
        assert_eq!(drop_slot(&rows, (251.0, 120.0), None), None);
        assert_eq!(drop_slot(&rows, (9.0, 120.0), None), None);
        assert_eq!(drop_slot(&rows, (250.0, 120.0), None), Some(1));
        assert_eq!(drop_slot(&[], (100.0, 120.0), None), None);
    }

    #[test]
    fn rf_esp_16_the_gap_shown_holds_until_the_pointer_is_clearly_past() {
        let rows = rows(3);
        // Gap 1 is between the middles at 115 and 145.
        assert_eq!(drop_slot(&rows, (100.0, 113.0), Some(1)), Some(1));
        assert_eq!(drop_slot(&rows, (100.0, 110.0), Some(1)), Some(0));
        assert_eq!(drop_slot(&rows, (100.0, 148.0), Some(1)), Some(1));
        assert_eq!(drop_slot(&rows, (100.0, 150.0), Some(1)), Some(2));
        // A gap that no longer exists is not held.
        assert_eq!(drop_slot(&rows, (100.0, 150.0), Some(7)), Some(2));
    }

    #[test]
    fn rf_esp_16_a_drop_lands_where_the_line_was_drawn() {
        // Tabs a b c d; dragging b (1).
        assert_eq!(landing(1, 0), Some(0), "above a");
        assert_eq!(landing(1, 1), None, "just above itself");
        assert_eq!(landing(1, 2), None, "just below itself");
        assert_eq!(landing(1, 3), Some(2), "between c and d");
        assert_eq!(landing(1, 4), Some(3), "below d");
        assert_eq!(landing(0, 0), None);
        assert_eq!(landing(3, 0), Some(0));
    }

    #[test]
    fn rf_esp_16_rnf_16_the_keyboard_move_at_either_end_does_nothing() {
        assert_eq!(step(0, 3, false), None);
        assert_eq!(step(2, 3, true), None);
        assert_eq!(step(0, 1, true), None);
        assert_eq!(step(0, 1, false), None);
        assert_eq!(step(1, 3, false), Some(0));
        assert_eq!(step(1, 3, true), Some(2));
        assert_eq!(step(5, 3, false), None, "a place past the end");
    }

    #[test]
    fn rf_esp_16_a_drag_near_an_end_of_the_list_scrolls_it() {
        let (top, bottom) = (100.0, 500.0);
        let list = Span {
            left: 0.0,
            right: 264.0,
            top,
            bottom,
        };
        let at = |y: f32| scroll_step((100.0, y), list);
        assert_eq!(at(300.0), 0.0);
        assert_eq!(at(top + SCROLL_EDGE), 0.0);
        let near_top = at(top + 10.0);
        assert!(near_top > 0.0 && near_top < SCROLL_STEP);
        assert_eq!(at(top), SCROLL_STEP);
        assert_eq!(at(top - 50.0), SCROLL_STEP);
        assert_eq!(at(bottom), -SCROLL_STEP);
        assert!(at(bottom - 10.0) < 0.0);
        assert_eq!(at(f32::NAN), 0.0);
        let flat = Span {
            bottom: top,
            ..list
        };
        assert_eq!(scroll_step((100.0, 120.0), flat), 0.0);
    }

    #[test]
    fn rf_esp_16_a_drag_over_the_panes_does_not_scroll_the_tab_list() {
        let list = Span {
            left: 0.0,
            right: 264.0,
            top: 100.0,
            bottom: 500.0,
        };
        assert_eq!(scroll_step((600.0, 100.0), list), 0.0);
        assert_eq!(scroll_step((600.0, 500.0), list), 0.0);
        assert_eq!(scroll_step((-1.0, 100.0), list), 0.0);
        assert_eq!(scroll_step((264.0, 100.0), list), SCROLL_STEP);
    }

    #[test]
    fn rf_esp_16_scrolling_stops_at_both_ends_of_the_list() {
        assert_eq!(scrolled(0.0, 10.0, 200.0), 0.0);
        assert_eq!(scrolled(-50.0, 10.0, 200.0), -40.0);
        assert_eq!(scrolled(-195.0, -10.0, 200.0), -200.0);
        assert_eq!(scrolled(0.0, -10.0, 0.0), 0.0, "a list that fits");
    }

    #[test]
    fn a_row_moved_by_a_scroll_keeps_its_height() {
        let row = Span {
            left: 0.0,
            right: 200.0,
            top: 10.0,
            bottom: 40.0,
        };
        assert_eq!(
            row.shifted(-5.0),
            Span {
                top: 5.0,
                bottom: 35.0,
                ..row
            }
        );
    }
}
