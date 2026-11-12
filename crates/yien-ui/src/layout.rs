//! Where each pane of a tab goes, from the layout tree the core sends.

use yien_proto::{Direction, LayoutChild, LayoutNode, Orientation, PaneId};

/// A rectangle as fractions of the pane area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// The whole area.
    pub const FULL: Self = Self {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
}

/// Each pane with its rectangle, in tree order. Children share their split
/// in proportion to `flex`; unusable weights fall back to equal shares.
pub(crate) fn pane_rects(node: &LayoutNode) -> Vec<(PaneId, Rect)> {
    let mut out = Vec::new();
    place(node, Rect::FULL, &mut out);
    out
}

fn place(node: &LayoutNode, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match node {
        LayoutNode::Pane(pane) => out.push((*pane, area)),
        LayoutNode::Split {
            orientation,
            children,
        } => {
            let mut offset = 0.0;
            for (child, share) in children.iter().zip(shares(children)) {
                place(&child.node, part(area, *orientation, offset, share), out);
                offset += share;
            }
        }
    }
}

/// The part of its split each child takes, in proportion to `flex`;
/// unusable weights fall back to equal parts.
fn shares(children: &[LayoutChild]) -> Vec<f32> {
    let usable = |f: f32| f.is_finite() && f > 0.0;
    let total: f32 = children.iter().map(|c| c.flex).filter(|f| usable(*f)).sum();
    let equal = total <= 0.0 || children.iter().any(|c| !usable(c.flex));
    let count = children.len() as f32;
    children
        .iter()
        .map(|c| if equal { 1.0 / count } else { c.flex / total })
        .collect()
}

/// The slice of `area` from `offset` to `offset + share` along the split.
fn part(area: Rect, orientation: Orientation, offset: f32, share: f32) -> Rect {
    match orientation {
        Orientation::Horizontal => Rect {
            x: area.x + offset * area.w,
            y: area.y,
            w: share * area.w,
            h: area.h,
        },
        Orientation::Vertical => Rect {
            x: area.x,
            y: area.y + offset * area.h,
            w: area.w,
            h: share * area.h,
        },
    }
}

/// The boundary between two neighbouring children of a split (RF-PAN-02).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Divider {
    /// The first pane of the child before it; any pane of that child names
    /// the divider to the core.
    pub before: PaneId,
    /// The first pane of the child after it.
    pub after: PaneId,
    /// The split's: `Horizontal` children sit side by side, so the divider
    /// is an upright line that moves left and right.
    pub orientation: Orientation,
    /// The line, along the split's axis, as a fraction of the pane area.
    pub at: f32,
    /// Where the line starts across the axis and how long it is, as
    /// fractions of the pane area.
    pub across: (f32, f32),
    /// The split's length along its axis, as a fraction of the pane area.
    pub split: f32,
    /// The two children's length together, as a fraction of the pane area.
    pub pair: f32,
    /// The first child's part of the pair.
    pub share: f32,
    /// The least length the child before can be given along the axis, in
    /// the unit of the `min_leaf` the list was made with.
    pub min_before: f32,
    /// The same for the child after.
    pub min_after: f32,
}

/// Every divider of the layout, those inside a child before the one after
/// it, so the list reads left to right and top to bottom. `min_leaf` is the
/// least width and height of one pane.
pub(crate) fn dividers(node: &LayoutNode, min_leaf: (f32, f32)) -> Vec<Divider> {
    let mut out = Vec::new();
    find_dividers(node, Rect::FULL, min_leaf, &mut out);
    out
}

fn find_dividers(node: &LayoutNode, area: Rect, min_leaf: (f32, f32), out: &mut Vec<Divider>) {
    let LayoutNode::Split {
        orientation,
        children,
    } = node
    else {
        return;
    };
    let shares = shares(children);
    let (start, split, across) = match orientation {
        Orientation::Horizontal => (area.x, area.w, (area.y, area.h)),
        Orientation::Vertical => (area.y, area.h, (area.x, area.w)),
    };
    let mut offset = 0.0;
    for (i, child) in children.iter().enumerate() {
        let rect = part(area, *orientation, offset, shares[i]);
        find_dividers(&child.node, rect, min_leaf, out);
        offset += shares[i];
        let Some(next) = children.get(i + 1) else {
            break;
        };
        let (Some(before), Some(after)) = (first_pane(&child.node), first_pane(&next.node)) else {
            continue;
        };
        let pair = shares[i] + shares[i + 1];
        out.push(Divider {
            before,
            after,
            orientation: *orientation,
            at: start + offset * split,
            across,
            split,
            pair: pair * split,
            share: shares[i] / pair,
            min_before: min_length(&child.node, *orientation, min_leaf),
            min_after: min_length(&next.node, *orientation, min_leaf),
        });
    }
}

fn first_pane(node: &LayoutNode) -> Option<PaneId> {
    match node {
        LayoutNode::Pane(pane) => Some(*pane),
        LayoutNode::Split { children, .. } => children.iter().find_map(|c| first_pane(&c.node)),
    }
}

/// The least length `node` can take along `axis` with no pane under
/// `min_leaf`. Moving a divider scales everything on each side in
/// proportion, so a child of a split along the same axis binds at its
/// minimum over its part; across the axis each child spans the whole.
pub(crate) fn min_length(node: &LayoutNode, axis: Orientation, min_leaf: (f32, f32)) -> f32 {
    match node {
        LayoutNode::Pane(_) => match axis {
            Orientation::Horizontal => min_leaf.0,
            Orientation::Vertical => min_leaf.1,
        },
        LayoutNode::Split {
            orientation,
            children,
        } => children
            .iter()
            .zip(shares(children))
            .map(|(child, share)| {
                let least = min_length(&child.node, axis, min_leaf);
                if *orientation == axis {
                    least / share
                } else {
                    least
                }
            })
            .fold(0.0, f32::max),
    }
}

/// The first child's part of the pair once the divider moved `moved` from
/// where `start` put it, with neither child under its minimum. `area` is
/// the pane area's length along the axis, in the unit of the minimums.
/// `None` when the pair cannot hold both minimums or a number is unusable.
pub(crate) fn dragged_share(divider: &Divider, start: f32, moved: f32, area: f32) -> Option<f32> {
    let pair = divider.pair * area;
    if !(pair.is_finite() && pair > 0.0 && start.is_finite() && moved.is_finite()) {
        return None;
    }
    let (lo, hi) = (divider.min_before, pair - divider.min_after);
    if lo.is_nan() || hi.is_nan() || lo > hi {
        return None;
    }
    // max/min, not clamp: clamp panics when lo > hi.
    let length = (start * pair + moved).max(lo).min(hi);
    let share = length / pair;
    (share > 0.0 && share < 1.0).then_some(share)
}

/// The layout with the divider between the children holding `before` and
/// `after` moved so the first takes `share` of the pair: what a drag shows
/// until the core answers. `None` when they are not on its two sides.
pub(crate) fn with_share(
    node: &LayoutNode,
    before: PaneId,
    after: PaneId,
    share: f32,
) -> Option<LayoutNode> {
    let LayoutNode::Split {
        orientation,
        children,
    } = node
    else {
        return None;
    };
    let holds = |n: &LayoutNode, p: PaneId| n.panes().contains(&p);
    let parts = shares(children);
    let mut moved = children.clone();
    for i in 0..children.len().saturating_sub(1) {
        if holds(&children[i].node, before) && holds(&children[i + 1].node, after) {
            let pair = parts[i] + parts[i + 1];
            for (child, part) in moved.iter_mut().zip(&parts) {
                child.flex = *part;
            }
            moved[i].flex = pair * share;
            moved[i + 1].flex = pair - pair * share;
            return Some(LayoutNode::Split {
                orientation: *orientation,
                children: moved,
            });
        }
    }
    let at = children
        .iter()
        .position(|c| holds(&c.node, before) && holds(&c.node, after))?;
    moved[at].node = with_share(&children[at].node, before, after, share)?;
    Some(LayoutNode::Split {
        orientation: *orientation,
        children: moved,
    })
}

/// The pane next to `from` in `direction`: the nearest one past its edge,
/// preferring the one that overlaps it most along the other axis.
pub(crate) fn neighbor(
    rects: &[(PaneId, Rect)],
    from: PaneId,
    direction: Direction,
) -> Option<PaneId> {
    const EPS: f32 = 1e-4;
    let (_, a) = rects.iter().find(|(id, _)| *id == from)?;
    let overlap = |lo1: f32, hi1: f32, lo2: f32, hi2: f32| hi1.min(hi2) - lo1.max(lo2);
    rects
        .iter()
        .filter(|(id, _)| *id != from)
        .filter_map(|(id, b)| {
            let (gap, shared) = match direction {
                Direction::Right => (b.x - (a.x + a.w), overlap(a.y, a.y + a.h, b.y, b.y + b.h)),
                Direction::Left => (a.x - (b.x + b.w), overlap(a.y, a.y + a.h, b.y, b.y + b.h)),
                Direction::Down => (b.y - (a.y + a.h), overlap(a.x, a.x + a.w, b.x, b.x + b.w)),
                Direction::Up => (a.y - (b.y + b.h), overlap(a.x, a.x + a.w, b.x, b.x + b.w)),
            };
            (gap > -EPS && shared > EPS).then_some((*id, gap, shared))
        })
        .min_by(|x, y| {
            x.1.total_cmp(&y.1)
                .then_with(|| y.2.total_cmp(&x.2))
                .then_with(|| x.0.cmp(&y.0))
        })
        .map(|(id, _, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(n: u64) -> LayoutNode {
        LayoutNode::Pane(PaneId::new(n))
    }

    fn split(orientation: Orientation, children: Vec<(f32, LayoutNode)>) -> LayoutNode {
        LayoutNode::Split {
            orientation,
            children: children
                .into_iter()
                .map(|(flex, node)| LayoutChild { flex, node })
                .collect(),
        }
    }

    /// 1 | (2 over 3), with 1 taking a quarter.
    fn sample() -> LayoutNode {
        split(
            Orientation::Horizontal,
            vec![
                (1.0, pane(1)),
                (
                    3.0,
                    split(Orientation::Vertical, vec![(0.5, pane(2)), (0.5, pane(3))]),
                ),
            ],
        )
    }

    #[test]
    fn a_single_pane_fills_the_area() {
        assert_eq!(pane_rects(&pane(4)), vec![(PaneId::new(4), Rect::FULL)]);
    }

    #[test]
    fn rf_pan_01_splits_share_by_flex_in_tree_order() {
        let rects = pane_rects(&sample());
        let ids: Vec<u64> = rects.iter().map(|(p, _)| p.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(
            rects[0].1,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 0.25,
                h: 1.0
            }
        );
        assert_eq!(
            rects[2].1,
            Rect {
                x: 0.25,
                y: 0.5,
                w: 0.75,
                h: 0.5
            }
        );
        let area: f32 = rects.iter().map(|(_, r)| r.w * r.h).sum();
        assert!((area - 1.0).abs() < 1e-5);
    }

    #[test]
    fn bad_weights_fall_back_to_equal_shares() {
        let node = split(
            Orientation::Horizontal,
            vec![(f32::NAN, pane(1)), (2.0, pane(2))],
        );
        let rects = pane_rects(&node);
        assert_eq!(rects[0].1.w, 0.5);
        assert_eq!(rects[1].1.x, 0.5);
    }

    #[test]
    fn rf_pan_03_neighbors_follow_the_geometry() {
        let rects = pane_rects(&sample());
        let id = PaneId::new;
        assert_eq!(neighbor(&rects, id(1), Direction::Right), Some(id(2)));
        assert_eq!(neighbor(&rects, id(3), Direction::Left), Some(id(1)));
        assert_eq!(neighbor(&rects, id(2), Direction::Down), Some(id(3)));
        assert_eq!(neighbor(&rects, id(3), Direction::Up), Some(id(2)));
        assert_eq!(neighbor(&rects, id(1), Direction::Left), None);
        assert_eq!(neighbor(&rects, id(9), Direction::Left), None);
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn flexes(node: &LayoutNode) -> Vec<f32> {
        match node {
            LayoutNode::Pane(_) => Vec::new(),
            LayoutNode::Split { children, .. } => children.iter().map(|c| c.flex).collect(),
        }
    }

    const MIN: (f32, f32) = (50.0, 40.0);

    #[test]
    fn rf_pan_02_dividers_sit_between_neighbours_in_reading_order() {
        let found = dividers(&sample(), MIN);
        assert_eq!(found.len(), 2);
        let (outer, inner) = (found[0], found[1]);
        let id = PaneId::new;
        assert_eq!((outer.before, outer.after), (id(1), id(2)));
        assert_eq!(outer.orientation, Orientation::Horizontal);
        assert!(close(outer.at, 0.25));
        assert_eq!(outer.across, (0.0, 1.0));
        assert!(close(outer.share, 0.25));
        assert!(close(outer.pair, 1.0));
        assert_eq!((inner.before, inner.after), (id(2), id(3)));
        assert_eq!(inner.orientation, Orientation::Vertical);
        assert!(close(inner.at, 0.5));
        assert!(close(inner.across.0, 0.25) && close(inner.across.1, 0.75));
        assert!(close(inner.share, 0.5));
        assert!(dividers(&pane(1), MIN).is_empty());
    }

    #[test]
    fn rf_pan_02_a_drag_follows_the_pointer_and_stops_at_the_minimum() {
        let node = split(
            Orientation::Horizontal,
            vec![(0.5, pane(1)), (0.5, pane(2))],
        );
        let d = dividers(&node, MIN)[0];
        let area = 1000.0;
        assert!(close(dragged_share(&d, 0.5, 100.0, area).unwrap(), 0.6));
        assert!(close(dragged_share(&d, 0.5, -5000.0, area).unwrap(), 0.05));
        assert!(close(dragged_share(&d, 0.5, 5000.0, area).unwrap(), 0.95));
        assert_eq!(dragged_share(&d, 0.5, f32::NAN, area), None);
        assert_eq!(dragged_share(&d, 0.5, 10.0, 0.0), None);
        // A window too small for both panes: the divider stays.
        assert_eq!(dragged_share(&d, 0.5, 10.0, 80.0), None);
    }

    #[test]
    fn rf_pan_02_a_nested_split_keeps_every_pane_over_the_minimum() {
        // 1 | (2 | 3), the inner split a quarter and three quarters.
        let node = split(
            Orientation::Horizontal,
            vec![
                (0.5, pane(1)),
                (
                    0.5,
                    split(
                        Orientation::Horizontal,
                        vec![(0.25, pane(2)), (0.75, pane(3))],
                    ),
                ),
            ],
        );
        let outer = dividers(&node, MIN)
            .into_iter()
            .find(|d| d.before == PaneId::new(1))
            .unwrap();
        // Pane 2 holds a quarter of the right side, so the side needs 4 × 50.
        assert!(close(outer.min_after, 200.0));
        assert!(close(
            dragged_share(&outer, 0.5, 5000.0, 1000.0).unwrap(),
            0.8
        ));
        // Across the axis a column needs only one pane's width.
        let column = split(Orientation::Vertical, vec![(0.5, pane(2)), (0.5, pane(3))]);
        assert!(close(
            min_length(&column, Orientation::Horizontal, MIN),
            50.0
        ));
        assert!(close(min_length(&column, Orientation::Vertical, MIN), 80.0));
    }

    #[test]
    fn rf_pan_02_a_three_way_split_moves_only_the_two_neighbours() {
        let node = split(
            Orientation::Horizontal,
            vec![(1.0, pane(1)), (1.0, pane(2)), (1.0, pane(3))],
        );
        let moved = with_share(&node, PaneId::new(2), PaneId::new(3), 0.25).unwrap();
        let f = flexes(&moved);
        assert!(close(f[0], 1.0 / 3.0));
        assert!(close(f[1], 2.0 / 3.0 * 0.25));
        assert!(close(f[2], 2.0 / 3.0 * 0.75));
        let found = dividers(&moved, MIN);
        assert!(close(found[1].share, 0.25));
        assert!(close(found[0].at, 1.0 / 3.0));
    }

    #[test]
    fn rf_pan_02_a_preview_reaches_the_split_that_owns_the_divider() {
        let id = PaneId::new;
        let inner = with_share(&sample(), id(2), id(3), 0.2).unwrap();
        assert_eq!(flexes(&inner), flexes(&sample()));
        let LayoutNode::Split { children, .. } = &inner else {
            panic!("expected a split");
        };
        assert!(close(flexes(&children[1].node)[0], 0.2));
        // Any pane of the column names the outer divider's far side.
        let outer = with_share(&sample(), id(1), id(3), 0.5).unwrap();
        assert!(close(flexes(&outer)[0], 0.5));
        assert_eq!(with_share(&sample(), id(2), id(1), 0.5), None);
        assert_eq!(with_share(&sample(), id(1), id(9), 0.5), None);
        assert_eq!(with_share(&pane(1), id(1), id(2), 0.5), None);
    }
}
