//! Where each pane of a tab goes, from the layout tree the core sends.

use yien_proto::{Direction, LayoutNode, Orientation, PaneId};

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
            if children.is_empty() {
                return;
            }
            let usable = |f: f32| f.is_finite() && f > 0.0;
            let total: f32 = children.iter().map(|c| c.flex).filter(|f| usable(*f)).sum();
            let equal = total <= 0.0 || children.iter().any(|c| !usable(c.flex));
            let count = children.len() as f32;
            let mut offset = 0.0;
            for child in children {
                let share = if equal {
                    1.0 / count
                } else {
                    child.flex / total
                };
                let rect = match orientation {
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
                };
                offset += share;
                place(&child.node, rect, out);
            }
        }
    }
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
    use yien_proto::LayoutChild;

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
}
