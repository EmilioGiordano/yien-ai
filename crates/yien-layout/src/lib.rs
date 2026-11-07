//! The pane tree: branches hold children with flex ratios, leaves hold a pane.
//!
//! Modelled on Warp's design and i3/tmux before it. Knows nothing about
//! terminals, windows or pixels beyond the rectangle it is asked to fill, so
//! the whole thing is testable without opening a window. Imported from foothy.
//!
//! ```
//! use yien_layout::{Direction, Orientation, Rect, Tree};
//!
//! let mut tree = Tree::new();
//! let right = tree.split(0, Orientation::Horizontal).unwrap();
//! let area = Rect::new(0.0, 0.0, 100.0, 50.0);
//! assert_eq!(tree.layout(area)[&right].x, 50.0);
//! assert_eq!(tree.neighbour(0, Direction::Right, area), Some(right));
//! ```

#![forbid(unsafe_code)]

use std::collections::HashMap;

/// The divider under a point, if any.
///
/// Hit areas overlap where an inner branch meets an outer one; the smaller
/// rectangle wins because it is the more specific boundary the eye sees. Equal
/// areas fall back to the id, so the cursor icon and the click that follows it
/// can never pick different dividers.
///
/// ```
/// use yien_layout::{divider_at, Orientation, Rect, Tree};
///
/// let mut tree = Tree::new();
/// tree.split(0, Orientation::Horizontal);
/// let dividers = tree.dividers(Rect::new(0.0, 0.0, 100.0, 100.0), 6.0);
/// assert!(divider_at(&dividers, 50.0, 10.0).is_some());
/// assert!(divider_at(&dividers, 10.0, 10.0).is_none());
/// ```
pub fn divider_at(dividers: &[Divider], x: f32, y: f32) -> Option<Divider> {
    dividers
        .iter()
        .copied()
        .filter(|d| x >= d.rect.x && x <= d.rect.right() && y >= d.rect.y && y <= d.rect.bottom())
        .min_by(|a, b| {
            let area_of = |d: &Divider| d.rect.w * d.rect.h;
            area_of(a)
                .partial_cmp(&area_of(b))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| (a.id.branch, a.id.after).cmp(&(b.id.branch, b.id.after)))
        })
}

/// How much better a zone has to be before the preview leaves the one it is
/// already showing. The four zones meet at the centre of a pane, so without a
/// dead band a still hand flips the preview between them.
const ZONE_HYSTERESIS: f32 = 0.06;

/// A pane's handle inside one [`Tree`]. Handed out by the tree itself, starting
/// at 0 for the first pane; it carries no meaning outside that tree.
pub type PaneId = u64;
type NodeId = usize;

/// How a branch lays out its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// Children side by side, left to right.
    Horizontal,
    /// Children stacked, top to bottom.
    Vertical,
}

/// A direction to move focus in, from one pane to its neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Towards smaller x.
    Left,
    /// Towards larger x.
    Right,
    /// Towards smaller y.
    Up,
    /// Towards larger y.
    Down,
}

impl Direction {
    fn axis(self) -> Orientation {
        match self {
            Direction::Left | Direction::Right => Orientation::Horizontal,
            Direction::Up | Direction::Down => Orientation::Vertical,
        }
    }
}

/// Which side of a pane a dragged pane would land on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropZone {
    /// Docked on the left, taking the left half.
    Left,
    /// Docked on the right, taking the right half.
    Right,
    /// Docked on top, taking the top half.
    Top,
    /// Docked below, taking the bottom half.
    Bottom,
}

impl DropZone {
    fn orientation(self) -> Orientation {
        match self {
            DropZone::Left | DropZone::Right => Orientation::Horizontal,
            DropZone::Top | DropZone::Bottom => Orientation::Vertical,
        }
    }

    /// Whether the arriving pane goes before the target in its branch.
    fn before(self) -> bool {
        matches!(self, DropZone::Left | DropZone::Top)
    }
}

/// Identifies the gap between two adjacent children of one branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DividerId {
    branch: usize,
    /// The divider sits after this child.
    after: usize,
}

/// A draggable boundary between two panes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Divider {
    /// Pass it to [`Tree::drag_divider`] to move this boundary.
    pub id: DividerId,
    /// Hit target, already widened to `thickness`.
    pub rect: Rect,
    /// The axis it slides along: `Horizontal` moves left and right.
    pub orientation: Orientation,
}

/// An axis-aligned rectangle in whatever unit the caller uses (pixels, cells).
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
    /// A rectangle from its top-left corner and size.
    ///
    /// ```
    /// let r = yien_layout::Rect::new(1.0, 2.0, 3.0, 4.0);
    /// assert_eq!((r.x, r.y, r.w, r.h), (1.0, 2.0, 3.0, 4.0));
    /// ```
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn right(&self) -> f32 {
        self.x + self.w
    }

    fn bottom(&self) -> f32 {
        self.y + self.h
    }

    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

#[derive(Debug, Clone)]
enum Kind {
    Leaf(PaneId),
    Branch {
        orientation: Orientation,
        children: Vec<NodeId>,
        /// One per child, summing to 1.0.
        flex: Vec<f32>,
    },
}

#[derive(Debug, Clone)]
struct Node {
    parent: Option<NodeId>,
    kind: Kind,
}

/// The pane tree of one tab. It always holds at least one pane.
///
/// ```
/// use yien_layout::{DropZone, Orientation, Tree};
///
/// let mut tree = Tree::new();
/// let below = tree.split(0, Orientation::Vertical).unwrap();
/// let left = tree.split_towards(below, DropZone::Left).unwrap();
/// assert_eq!(tree.len(), 3);
/// assert!(tree.close(left));
/// assert_eq!(tree.panes(), vec![0, below]);
/// ```
#[derive(Debug, Clone)]
pub struct Tree {
    nodes: Vec<Node>,
    root: NodeId,
    /// Freed slots, so ids do not grow without bound as panes come and go.
    vacant: Vec<NodeId>,
    next_pane: PaneId,
}

impl Tree {
    /// A tree with a single pane filling everything.
    ///
    /// ```
    /// let tree = yien_layout::Tree::new();
    /// assert_eq!(tree.panes(), vec![0]);
    /// ```
    pub fn new() -> Self {
        let root = Node {
            parent: None,
            kind: Kind::Leaf(0),
        };
        Self {
            nodes: vec![root],
            root: 0,
            vacant: Vec::new(),
            next_pane: 1,
        }
    }

    /// Every pane, in tree order (left to right, top to bottom within a branch).
    ///
    /// ```
    /// use yien_layout::{DropZone, Tree};
    /// let mut tree = Tree::new();
    /// let left = tree.split_towards(0, DropZone::Left).unwrap();
    /// assert_eq!(tree.panes(), vec![left, 0]);
    /// ```
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(self.root, &mut out);
        out
    }

    fn collect(&self, node: NodeId, out: &mut Vec<PaneId>) {
        match &self.nodes[node].kind {
            Kind::Leaf(pane) => out.push(*pane),
            Kind::Branch { children, .. } => {
                for &c in children {
                    self.collect(c, out);
                }
            }
        }
    }

    /// How many panes the tree holds; never zero.
    ///
    /// ```
    /// let mut tree = yien_layout::Tree::new();
    /// tree.split(0, yien_layout::Orientation::Vertical);
    /// assert_eq!(tree.len(), 2);
    /// ```
    pub fn len(&self) -> usize {
        self.panes().len()
    }

    /// Always `false`: a tree always has a pane.
    ///
    /// ```
    /// assert!(!yien_layout::Tree::new().is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        false
    }

    fn find(&self, pane: PaneId) -> Option<NodeId> {
        (0..self.nodes.len()).find(|&i| {
            !self.vacant.contains(&i) && matches!(self.nodes[i].kind, Kind::Leaf(p) if p == pane)
        })
    }

    fn alloc(&mut self, node: Node) -> NodeId {
        match self.vacant.pop() {
            Some(slot) => {
                self.nodes[slot] = node;
                slot
            }
            None => {
                self.nodes.push(node);
                self.nodes.len() - 1
            }
        }
    }

    /// Splits `pane`, putting the new pane after it. Returns the new pane's id.
    ///
    /// When the target already sits in a branch of the same orientation the new
    /// pane joins that branch instead of nesting a new one; otherwise every
    /// split would deepen the tree and the ratios would stop being comparable.
    ///
    /// ```
    /// use yien_layout::{Orientation, Tree};
    /// let mut tree = Tree::new();
    /// let new = tree.split(0, Orientation::Horizontal).unwrap();
    /// assert_eq!(tree.panes(), vec![0, new]);
    /// assert_eq!(tree.split(99, Orientation::Horizontal), None);
    /// ```
    pub fn split(&mut self, pane: PaneId, orientation: Orientation) -> Option<PaneId> {
        let zone = match orientation {
            Orientation::Horizontal => DropZone::Right,
            Orientation::Vertical => DropZone::Bottom,
        };
        self.split_towards(pane, zone)
    }

    /// Splits `pane`, putting the new one on the side `zone` names. The new pane
    /// takes half of the one it came from.
    ///
    /// ```
    /// use yien_layout::{DropZone, Tree};
    /// let mut tree = Tree::new();
    /// let above = tree.split_towards(0, DropZone::Top).unwrap();
    /// assert_eq!(tree.panes(), vec![above, 0]);
    /// ```
    pub fn split_towards(&mut self, pane: PaneId, zone: DropZone) -> Option<PaneId> {
        let node = self.find(pane)?;
        let new_pane = self.next_pane;
        self.next_pane += 1;
        self.insert_beside(node, new_pane, zone);
        Some(new_pane)
    }

    /// Removes a pane. The last one cannot be closed: a tree always has a pane.
    ///
    /// ```
    /// use yien_layout::{Orientation, Tree};
    /// let mut tree = Tree::new();
    /// assert!(!tree.close(0));
    /// let new = tree.split(0, Orientation::Horizontal).unwrap();
    /// assert!(tree.close(new));
    /// ```
    pub fn close(&mut self, pane: PaneId) -> bool {
        if self.len() <= 1 {
            return false;
        }
        self.detach(pane)
    }

    /// Grows `pane` by `delta` (a fraction of its branch) at its siblings' cost.
    ///
    /// ```
    /// use yien_layout::{Orientation, Rect, Tree};
    /// let mut tree = Tree::new();
    /// tree.split(0, Orientation::Horizontal);
    /// assert!(tree.resize(0, 0.25));
    /// let area = Rect::new(0.0, 0.0, 100.0, 100.0);
    /// assert!((tree.layout(area)[&0].w - 75.0).abs() < 0.01);
    /// ```
    pub fn resize(&mut self, pane: PaneId, delta: f32) -> bool {
        let Some(node) = self.find(pane) else {
            return false;
        };
        let Some(parent) = self.nodes[node].parent else {
            return false;
        };
        const MIN: f32 = 0.05;

        if let Kind::Branch { children, flex, .. } = &mut self.nodes[parent].kind {
            let Some(at) = children.iter().position(|&c| c == node) else {
                return false;
            };
            let others: f32 = flex
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != at)
                .map(|(_, f)| f)
                .sum();
            let upper = 1.0 - MIN * (flex.len() - 1) as f32;
            // f32::clamp panics when min > max, which happens past 20 siblings.
            if !delta.is_finite() || upper < MIN {
                return false;
            }
            let target = (flex[at] + delta).clamp(MIN, upper);
            let applied = target - flex[at];
            if others <= 0.0 {
                return false;
            }
            flex[at] = target;
            for (i, f) in flex.iter_mut().enumerate() {
                if i != at {
                    *f -= applied * (*f / others);
                }
            }
            return true;
        }
        false
    }

    /// Every pane's rectangle inside `area`.
    ///
    /// ```
    /// use yien_layout::{Orientation, Rect, Tree};
    /// let mut tree = Tree::new();
    /// let below = tree.split(0, Orientation::Vertical).unwrap();
    /// let rects = tree.layout(Rect::new(0.0, 0.0, 80.0, 40.0));
    /// assert_eq!(rects[&below], Rect::new(0.0, 20.0, 80.0, 20.0));
    /// ```
    pub fn layout(&self, area: Rect) -> HashMap<PaneId, Rect> {
        let mut out = HashMap::new();
        self.place(self.root, area, &mut out);
        out
    }

    /// Rectangles of every node, branches included: dragging needs the branch's
    /// extent to turn a pixel delta into a flex fraction.
    fn node_rects(&self, area: Rect) -> HashMap<NodeId, Rect> {
        let mut out = HashMap::new();
        self.place_nodes(self.root, area, &mut out);
        out
    }

    fn place_nodes(&self, node: NodeId, area: Rect, out: &mut HashMap<NodeId, Rect>) {
        out.insert(node, area);
        if let Kind::Branch {
            orientation,
            children,
            flex,
        } = &self.nodes[node].kind
        {
            let mut offset = 0.0;
            for (i, &child) in children.iter().enumerate() {
                let last = i == children.len() - 1;
                let rect = child_rect(area, *orientation, flex[i], offset, last);
                offset += span_of(rect, *orientation);
                self.place_nodes(child, rect, out);
            }
        }
    }

    /// Where a pane dropped at this point would land, and the rectangle to
    /// preview. The nearest edge of the pane under the cursor wins.
    ///
    /// `current` is the zone already on screen, if any; it keeps its slot unless
    /// another beats it by [`ZONE_HYSTERESIS`].
    ///
    /// ```
    /// use yien_layout::{DropZone, Rect, Tree};
    /// let tree = Tree::new();
    /// let area = Rect::new(0.0, 0.0, 100.0, 100.0);
    /// let (pane, zone, preview) = tree.drop_target(95.0, 50.0, area, None).unwrap();
    /// assert_eq!((pane, zone), (0, DropZone::Right));
    /// assert_eq!(preview, Rect::new(50.0, 0.0, 50.0, 100.0));
    /// ```
    pub fn drop_target(
        &self,
        x: f32,
        y: f32,
        area: Rect,
        current: Option<(PaneId, DropZone)>,
    ) -> Option<(PaneId, DropZone, Rect)> {
        let rects = self.layout(area);
        let (&pane, &r) = rects
            .iter()
            .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)?;
        if r.w <= 0.0 || r.h <= 0.0 {
            return None;
        }

        let distance = |zone: DropZone| match zone {
            DropZone::Left => (x - r.x) / r.w,
            DropZone::Right => 1.0 - (x - r.x) / r.w,
            DropZone::Top => (y - r.y) / r.h,
            DropZone::Bottom => 1.0 - (y - r.y) / r.h,
        };

        let mut zone = DropZone::Left;
        let mut best = distance(DropZone::Left);
        for candidate in [DropZone::Right, DropZone::Top, DropZone::Bottom] {
            let d = distance(candidate);
            if d < best {
                best = d;
                zone = candidate;
            }
        }

        if let Some((held_pane, held)) = current
            && held_pane == pane
            && distance(held) <= best + ZONE_HYSTERESIS
        {
            zone = held;
        }

        let preview = match zone {
            DropZone::Left => Rect::new(r.x, r.y, r.w / 2.0, r.h),
            DropZone::Right => Rect::new(r.x + r.w / 2.0, r.y, r.w / 2.0, r.h),
            DropZone::Top => Rect::new(r.x, r.y, r.w, r.h / 2.0),
            DropZone::Bottom => Rect::new(r.x, r.y + r.h / 2.0, r.w, r.h / 2.0),
        };
        Some((pane, zone, preview))
    }

    /// Moves `pane` next to `target`. The pane keeps its identity, so whatever
    /// terminal it holds moves with it.
    ///
    /// ```
    /// use yien_layout::{DropZone, Orientation, Tree};
    /// let mut tree = Tree::new();
    /// let b = tree.split(0, Orientation::Horizontal).unwrap();
    /// assert!(tree.move_pane(b, 0, DropZone::Left));
    /// assert_eq!(tree.panes(), vec![b, 0]);
    /// ```
    pub fn move_pane(&mut self, pane: PaneId, target: PaneId, zone: DropZone) -> bool {
        if pane == target || self.find(pane).is_none() || self.find(target).is_none() {
            return false;
        }
        if !self.detach(pane) {
            return false;
        }
        // Detaching may have collapsed a branch, so the target is looked up again.
        let Some(target_node) = self.find(target) else {
            return false;
        };
        self.insert_beside(target_node, pane, zone);
        true
    }

    /// Removes a leaf without touching the pane's identity, collapsing a branch
    /// left with a single child.
    fn detach(&mut self, pane: PaneId) -> bool {
        let Some(node) = self.find(pane) else {
            return false;
        };
        let Some(parent) = self.nodes[node].parent else {
            return false;
        };
        let remaining = if let Kind::Branch { children, flex, .. } = &mut self.nodes[parent].kind {
            let Some(at) = children.iter().position(|&c| c == node) else {
                return false;
            };
            children.remove(at);
            let freed = flex.remove(at);
            let total: f32 = flex.iter().sum();
            if total > 0.0 {
                for f in flex.iter_mut() {
                    *f += freed * (*f / total);
                }
            }
            children.clone()
        } else {
            return false;
        };
        self.vacant.push(node);

        if remaining.len() == 1 {
            let only = remaining[0];
            let kind = self.nodes[only].kind.clone();
            self.nodes[parent].kind = kind;
            self.vacant.push(only);
            if let Kind::Branch { children, .. } = &self.nodes[parent].kind.clone() {
                for &c in children {
                    self.nodes[c].parent = Some(parent);
                }
            }
        }
        true
    }

    fn insert_beside(&mut self, target_node: NodeId, pane: PaneId, zone: DropZone) {
        let orientation = zone.orientation();
        let same_axis = self.nodes[target_node]
            .parent
            .and_then(|p| match &self.nodes[p].kind {
                Kind::Branch {
                    orientation: o,
                    children,
                    ..
                } if *o == orientation => children
                    .iter()
                    .position(|&c| c == target_node)
                    .map(|at| (p, at)),
                _ => None,
            });

        if let Some((parent, at)) = same_axis {
            let leaf = self.alloc(Node {
                parent: Some(parent),
                kind: Kind::Leaf(pane),
            });
            if let Kind::Branch { children, flex, .. } = &mut self.nodes[parent].kind {
                let half = flex[at] / 2.0;
                flex[at] = half;
                let slot = if zone.before() { at } else { at + 1 };
                flex.insert(slot, half);
                children.insert(slot, leaf);
            }
        } else {
            let existing = self.alloc(Node {
                parent: Some(target_node),
                kind: self.nodes[target_node].kind.clone(),
            });
            let arriving = self.alloc(Node {
                parent: Some(target_node),
                kind: Kind::Leaf(pane),
            });
            let children = if zone.before() {
                vec![arriving, existing]
            } else {
                vec![existing, arriving]
            };
            self.nodes[target_node].kind = Kind::Branch {
                orientation,
                children,
                flex: vec![0.5, 0.5],
            };
        }
    }

    /// Every draggable boundary, as hit targets `thickness` pixels wide.
    ///
    /// ```
    /// use yien_layout::{Orientation, Rect, Tree};
    /// let mut tree = Tree::new();
    /// tree.split(0, Orientation::Horizontal);
    /// let dividers = tree.dividers(Rect::new(0.0, 0.0, 100.0, 100.0), 6.0);
    /// assert_eq!(dividers.len(), 1);
    /// assert_eq!(dividers[0].rect, Rect::new(47.0, 0.0, 6.0, 100.0));
    /// ```
    pub fn dividers(&self, area: Rect, thickness: f32) -> Vec<Divider> {
        let rects = self.node_rects(area);
        let half = thickness / 2.0;
        let mut out = Vec::new();

        for (node, node_rect) in &rects {
            let Kind::Branch {
                orientation,
                children,
                flex,
            } = &self.nodes[*node].kind
            else {
                continue;
            };
            let mut offset = 0.0;
            // Every child but the last: the last one has no boundary after it.
            for (i, share) in flex
                .iter()
                .enumerate()
                .take(children.len().saturating_sub(1))
            {
                let rect = child_rect(*node_rect, *orientation, *share, offset, false);
                offset += span_of(rect, *orientation);
                let hit = match orientation {
                    Orientation::Horizontal => Rect::new(
                        node_rect.x + offset - half,
                        node_rect.y,
                        thickness,
                        node_rect.h,
                    ),
                    Orientation::Vertical => Rect::new(
                        node_rect.x,
                        node_rect.y + offset - half,
                        node_rect.w,
                        thickness,
                    ),
                };
                out.push(Divider {
                    id: DividerId {
                        branch: *node,
                        after: i,
                    },
                    rect: hit,
                    orientation: *orientation,
                });
            }
        }
        // node_rects is a HashMap, so iteration order changes between calls.
        // Without this the same point can grab different dividers on different
        // frames, and the cursor icon disagrees with what the click grabs.
        out.sort_by_key(|d| (d.id.branch, d.id.after));
        out
    }

    /// The divider under a point, if any. Callers holding a cached set should
    /// use [`divider_at`] directly instead of rebuilding the layout.
    ///
    /// ```
    /// use yien_layout::{Orientation, Rect, Tree};
    /// let mut tree = Tree::new();
    /// tree.split(0, Orientation::Vertical);
    /// let area = Rect::new(0.0, 0.0, 100.0, 100.0);
    /// assert!(tree.divider_at(10.0, 51.0, area, 6.0).is_some());
    /// assert!(tree.divider_at(10.0, 10.0, area, 6.0).is_none());
    /// ```
    pub fn divider_at(&self, x: f32, y: f32, area: Rect, thickness: f32) -> Option<Divider> {
        divider_at(&self.dividers(area, thickness), x, y)
    }

    /// Moves one boundary by `delta` pixels.
    ///
    /// Unlike [`Tree::resize`], the space comes from the neighbour across that
    /// boundary alone; the rest of the branch stays put, which is what dragging
    /// a divider looks like.
    ///
    /// ```
    /// use yien_layout::{Orientation, Rect, Tree};
    /// let mut tree = Tree::new();
    /// let b = tree.split(0, Orientation::Horizontal).unwrap();
    /// let area = Rect::new(0.0, 0.0, 100.0, 100.0);
    /// let id = tree.dividers(area, 6.0)[0].id;
    /// assert!(tree.drag_divider(id, -20.0, area));
    /// assert!((tree.layout(area)[&b].w - 70.0).abs() < 0.01);
    /// ```
    pub fn drag_divider(&mut self, id: DividerId, delta: f32, area: Rect) -> bool {
        let Some(branch_rect) = self.node_rects(area).get(&id.branch).copied() else {
            return false;
        };
        let Kind::Branch {
            orientation, flex, ..
        } = &mut self.nodes[id.branch].kind
        else {
            return false;
        };
        if id.after + 1 >= flex.len() {
            return false;
        }
        let extent = span_of(branch_rect, *orientation);
        if !extent.is_finite() || extent <= 0.0 || !delta.is_finite() {
            return false;
        }

        const MIN: f32 = 0.05;
        let (a, b) = (id.after, id.after + 1);
        // Clamped so neither neighbour is squeezed out of existence. Both may
        // already sit below MIN after many splits; then there is no room to move.
        let (lo, hi) = (MIN - flex[a], flex[b] - MIN);
        if lo > hi {
            return false;
        }
        // max/min, not clamp: clamp panics on a NaN bound.
        let applied = (delta / extent).max(lo).min(hi);
        if applied.abs() < f32::EPSILON {
            return false;
        }
        flex[a] += applied;
        flex[b] -= applied;
        true
    }

    fn place(&self, node: NodeId, area: Rect, out: &mut HashMap<PaneId, Rect>) {
        match &self.nodes[node].kind {
            Kind::Leaf(pane) => {
                out.insert(*pane, area);
            }
            Kind::Branch {
                orientation,
                children,
                flex,
            } => {
                let mut offset = 0.0;
                for (i, &child) in children.iter().enumerate() {
                    let last = i == children.len() - 1;
                    let rect = child_rect(area, *orientation, flex[i], offset, last);
                    offset += span_of(rect, *orientation);
                    self.place(child, rect, out);
                }
            }
        }
    }

    /// The pane to focus moving `direction` from `pane`, chosen by geometry.
    ///
    /// ```
    /// use yien_layout::{Direction, Orientation, Rect, Tree};
    /// let mut tree = Tree::new();
    /// let below = tree.split(0, Orientation::Vertical).unwrap();
    /// let area = Rect::new(0.0, 0.0, 100.0, 100.0);
    /// assert_eq!(tree.neighbour(0, Direction::Down, area), Some(below));
    /// assert_eq!(tree.neighbour(0, Direction::Up, area), None);
    /// ```
    pub fn neighbour(&self, pane: PaneId, direction: Direction, area: Rect) -> Option<PaneId> {
        let rects = self.layout(area);
        let from = *rects.get(&pane)?;
        const EPS: f32 = 0.01;

        let mut best: Option<(PaneId, f32, f32)> = None;
        for (&other, rect) in &rects {
            if other == pane {
                continue;
            }
            let beyond = match direction {
                Direction::Left => rect.right() <= from.x + EPS,
                Direction::Right => rect.x >= from.right() - EPS,
                Direction::Up => rect.bottom() <= from.y + EPS,
                Direction::Down => rect.y >= from.bottom() - EPS,
            };
            if !beyond {
                continue;
            }

            let overlap = match direction.axis() {
                Orientation::Horizontal => {
                    (from.bottom().min(rect.bottom()) - from.y.max(rect.y)).max(0.0)
                }
                Orientation::Vertical => {
                    (from.right().min(rect.right()) - from.x.max(rect.x)).max(0.0)
                }
            };
            if overlap <= 0.0 {
                continue;
            }

            let (fx, fy) = from.center();
            let (ox, oy) = rect.center();
            let distance = match direction.axis() {
                Orientation::Horizontal => (ox - fx).abs(),
                Orientation::Vertical => (oy - fy).abs(),
            };

            let better = match best {
                None => true,
                Some((_, bo, bd)) => overlap > bo + EPS || (overlap > bo - EPS && distance < bd),
            };
            if better {
                best = Some((other, overlap, distance));
            }
        }
        best.map(|(id, _, _)| id)
    }
}

fn span_of(rect: Rect, orientation: Orientation) -> f32 {
    match orientation {
        Orientation::Horizontal => rect.w,
        Orientation::Vertical => rect.h,
    }
}

/// The last child absorbs the rounding, so the area is exactly filled.
fn child_rect(area: Rect, orientation: Orientation, share: f32, offset: f32, last: bool) -> Rect {
    match orientation {
        Orientation::Horizontal => {
            let w = if last {
                area.w - offset
            } else {
                area.w * share
            };
            Rect::new(area.x + offset, area.y, w, area.h)
        }
        Orientation::Vertical => {
            let h = if last {
                area.h - offset
            } else {
                area.h * share
            };
            Rect::new(area.x, area.y + offset, area.w, h)
        }
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

/// The structure of a [`Tree`] as plain data, so it can be described outside
/// the tree (for example in a protocol message).
///
/// ```
/// use yien_layout::{Orientation, Shape, Tree};
///
/// let mut tree = Tree::new();
/// let right = tree.split(0, Orientation::Horizontal).unwrap();
/// let Shape::Split { orientation, children } = tree.shape() else { unreachable!() };
/// assert_eq!(orientation, Orientation::Horizontal);
/// assert_eq!(children, vec![(0.5, Shape::Pane(0)), (0.5, Shape::Pane(right))]);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// A leaf holding one pane.
    Pane(PaneId),
    /// A branch: each child with its flex ratio; the ratios sum to 1.
    Split {
        /// How the children are laid out.
        orientation: Orientation,
        /// The children in order, each with its flex ratio.
        children: Vec<(f32, Shape)>,
    },
}

impl Tree {
    /// The tree's structure, from the root down.
    pub fn shape(&self) -> Shape {
        self.shape_of(self.root)
    }

    /// Rebuilds a tree from its structure, keeping each leaf's pane id and
    /// each branch's ratios. Ratios that drift from summing to 1 by more than
    /// a rounding error are scaled back to 1.
    ///
    /// `None` when the shape cannot be a tree: a branch with fewer than two
    /// children, a ratio that is not a positive finite number, or a pane id
    /// used twice.
    ///
    /// ```
    /// use yien_layout::{Orientation, Shape, Tree};
    ///
    /// let mut tree = Tree::new();
    /// let right = tree.split(0, Orientation::Horizontal).unwrap();
    /// tree.split(right, Orientation::Vertical);
    /// tree.resize(0, 0.2);
    /// let copy = Tree::from_shape(&tree.shape()).unwrap();
    /// assert_eq!(copy.shape(), tree.shape());
    ///
    /// let empty = Shape::Split { orientation: Orientation::Vertical, children: Vec::new() };
    /// assert!(Tree::from_shape(&empty).is_none());
    /// ```
    pub fn from_shape(shape: &Shape) -> Option<Tree> {
        let mut tree = Tree {
            nodes: Vec::new(),
            root: 0,
            vacant: Vec::new(),
            next_pane: 0,
        };
        let mut seen = Vec::new();
        tree.root = tree.build(shape, None, &mut seen)?;
        tree.next_pane = seen.iter().max().map_or(0, |max| max + 1);
        Some(tree)
    }

    fn build(
        &mut self,
        shape: &Shape,
        parent: Option<NodeId>,
        seen: &mut Vec<PaneId>,
    ) -> Option<NodeId> {
        match shape {
            Shape::Pane(pane) => {
                if seen.contains(pane) || *pane == PaneId::MAX {
                    return None;
                }
                seen.push(*pane);
                Some(self.alloc(Node {
                    parent,
                    kind: Kind::Leaf(*pane),
                }))
            }
            Shape::Split {
                orientation,
                children,
            } => {
                let total: f32 = children.iter().map(|(f, _)| f).sum();
                let valid = children.len() >= 2
                    && children.iter().all(|(f, _)| f.is_finite() && *f > 0.0)
                    && total.is_finite();
                if !valid {
                    return None;
                }
                let id = self.alloc(Node {
                    parent,
                    kind: Kind::Leaf(PaneId::MAX),
                });
                let mut ids = Vec::with_capacity(children.len());
                for (_, child) in children {
                    ids.push(self.build(child, Some(id), seen)?);
                }
                let scale = if (total - 1.0).abs() > 1e-3 {
                    total
                } else {
                    1.0
                };
                self.nodes[id].kind = Kind::Branch {
                    orientation: *orientation,
                    children: ids,
                    flex: children.iter().map(|(f, _)| f / scale).collect(),
                };
                Some(id)
            }
        }
    }

    fn shape_of(&self, node: NodeId) -> Shape {
        match &self.nodes[node].kind {
            Kind::Leaf(pane) => Shape::Pane(*pane),
            Kind::Branch {
                orientation,
                children,
                flex,
            } => Shape::Split {
                orientation: *orientation,
                children: children
                    .iter()
                    .zip(flex)
                    .map(|(&c, &f)| (f, self.shape_of(c)))
                    .collect(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };

    fn assert_close(a: f32, b: f32) {
        assert!((a - b).abs() < 0.01, "{a} != {b}");
    }

    fn shape_panes(shape: &Shape, out: &mut Vec<PaneId>) {
        match shape {
            Shape::Pane(p) => out.push(*p),
            Shape::Split { children, .. } => {
                for (_, child) in children {
                    shape_panes(child, out);
                }
            }
        }
    }

    #[test]
    fn shape_follows_nesting_flex_and_order() {
        let mut tree = Tree::new();
        let right = tree.split(0, Orientation::Horizontal).unwrap();
        let below = tree.split(right, Orientation::Vertical).unwrap();
        assert!(tree.resize(0, 0.1));
        let Shape::Split {
            orientation,
            children,
        } = tree.shape()
        else {
            panic!("expected a split");
        };
        assert_eq!(orientation, Orientation::Horizontal);
        assert_eq!(children.len(), 2);
        assert_close(children[0].0, 0.6);
        assert_close(children[1].0, 0.4);
        assert_eq!(children[0].1, Shape::Pane(0));
        assert_eq!(
            children[1].1,
            Shape::Split {
                orientation: Orientation::Vertical,
                children: vec![(0.5, Shape::Pane(right)), (0.5, Shape::Pane(below))],
            }
        );
        let mut panes = Vec::new();
        shape_panes(&tree.shape(), &mut panes);
        assert_eq!(panes, tree.panes());
    }

    #[test]
    fn shape_of_a_single_pane_is_a_leaf() {
        assert_eq!(Tree::new().shape(), Shape::Pane(0));
    }

    #[test]
    fn starts_with_one_pane_filling_everything() {
        let t = Tree::new();
        assert_eq!(t.panes(), vec![0]);
        let l = t.layout(AREA);
        assert_eq!(l[&0], AREA);
    }

    #[test]
    fn rf_pan_01_horizontal_split_puts_panes_side_by_side() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let l = t.layout(AREA);
        assert_close(l[&0].w, 50.0);
        assert_close(l[&b].w, 50.0);
        assert_close(l[&0].h, 100.0);
        assert_close(l[&0].x, 0.0);
        assert_close(l[&b].x, 50.0);
    }

    #[test]
    fn rf_pan_01_vertical_split_stacks_panes() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Vertical).unwrap();
        let l = t.layout(AREA);
        assert_close(l[&0].h, 50.0);
        assert_close(l[&b].y, 50.0);
        assert_close(l[&0].w, 100.0);
    }

    #[test]
    fn splitting_on_the_same_axis_does_not_nest() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Horizontal).unwrap();
        let l = t.layout(AREA);
        assert_close(l[&0].w, 50.0);
        assert_close(l[&b].w, 25.0);
        assert_close(l[&c].w, 25.0);
    }

    #[test]
    fn layout_covers_the_area_without_gaps_or_overlap() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();
        let d = t.split(c, Orientation::Horizontal).unwrap();
        let l = t.layout(AREA);
        assert_eq!(l.len(), 4);

        let total: f32 = l.values().map(|r| r.w * r.h).sum();
        assert_close(total, AREA.w * AREA.h);

        let rects: Vec<&Rect> = l.values().collect();
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let overlap = (a.right().min(b.right()) - a.x.max(b.x)).max(0.0)
                    * (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0.0);
                assert_close(overlap, 0.0);
            }
        }
        let _ = d;
    }

    #[test]
    fn closing_gives_the_space_back() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        assert!(t.close(b));
        assert_eq!(t.panes(), vec![0]);
        assert_eq!(t.layout(AREA)[&0], AREA);
    }

    #[test]
    fn closing_collapses_a_branch_left_with_one_child() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();
        assert!(t.close(c));
        let l = t.layout(AREA);
        assert_eq!(l.len(), 2);
        assert_close(l[&b].h, 100.0);
    }

    #[test]
    fn the_last_pane_cannot_be_closed() {
        let mut t = Tree::new();
        assert!(!t.close(0));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn resize_moves_space_between_siblings() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        assert!(t.resize(0, 0.2));
        let l = t.layout(AREA);
        assert_close(l[&0].w, 70.0);
        assert_close(l[&b].w, 30.0);
        assert_close(l[&0].w + l[&b].w, 100.0);
    }

    #[test]
    fn resize_will_not_starve_a_pane() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        t.resize(0, 5.0);
        let l = t.layout(AREA);
        assert!(l[&b].w > 1.0, "the sibling ended up at {}", l[&b].w);
    }

    #[test]
    fn splitting_towards_a_side_puts_the_new_pane_there() {
        for (zone, expect_before) in [
            (DropZone::Left, true),
            (DropZone::Right, false),
            (DropZone::Top, true),
            (DropZone::Bottom, false),
        ] {
            let mut t = Tree::new();
            let new = t.split_towards(0, zone).unwrap();
            let l = t.layout(AREA);
            let (old, fresh) = (l[&0], l[&new]);
            match zone {
                DropZone::Left | DropZone::Right => {
                    assert_close(old.w, 50.0);
                    assert_close(fresh.w, 50.0);
                    if expect_before {
                        assert!(fresh.x < old.x, "{zone:?}: should land before");
                    } else {
                        assert!(fresh.x > old.x, "{zone:?}: should land after");
                    }
                }
                DropZone::Top | DropZone::Bottom => {
                    assert_close(old.h, 50.0);
                    assert_close(fresh.h, 50.0);
                    if expect_before {
                        assert!(fresh.y < old.y, "{zone:?}: should land above");
                    } else {
                        assert!(fresh.y > old.y, "{zone:?}: should land below");
                    }
                }
            }
        }
    }

    #[test]
    fn splitting_left_then_right_keeps_the_original_in_the_middle() {
        let mut t = Tree::new();
        let left = t.split_towards(0, DropZone::Left).unwrap();
        let right = t.split_towards(0, DropZone::Right).unwrap();
        let l = t.layout(AREA);
        assert!(l[&left].x < l[&0].x, "left");
        assert!(l[&right].x > l[&0].x, "right");
        assert_close(l[&left].w + l[&0].w + l[&right].w, 100.0);
    }

    #[test]
    fn divider_at_is_stable_across_calls() {
        // A T-junction: the outer boundary and an inner one overlap near the
        // crossing, and node_rects is a HashMap, so order used to vary per call.
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        t.split(b, Orientation::Vertical);

        let first = t.divider_at(50.0, 50.0, AREA, 8.0).map(|d| d.id);
        for _ in 0..50 {
            assert_eq!(t.divider_at(50.0, 50.0, AREA, 8.0).map(|d| d.id), first);
        }

        let ids: Vec<_> = t.dividers(AREA, 8.0).iter().map(|d| d.id).collect();
        for _ in 0..20 {
            let again: Vec<_> = t.dividers(AREA, 8.0).iter().map(|d| d.id).collect();
            assert_eq!(again, ids, "dividers() must keep a stable order");
        }
    }

    // --- moving a pane elsewhere in the layout ---

    #[test]
    fn rf_pan_05_drop_target_picks_the_nearest_edge() {
        let mut t = Tree::new();
        t.split(0, Orientation::Horizontal);
        // Inside the left pane (0..50 x 0..100).
        let (pane, zone, _) = t.drop_target(5.0, 50.0, AREA, None).unwrap();
        assert_eq!(pane, 0);
        assert_eq!(zone, DropZone::Left);

        let (_, zone, _) = t.drop_target(45.0, 50.0, AREA, None).unwrap();
        assert_eq!(zone, DropZone::Right);

        let (_, zone, _) = t.drop_target(25.0, 3.0, AREA, None).unwrap();
        assert_eq!(zone, DropZone::Top);

        let (_, zone, _) = t.drop_target(25.0, 97.0, AREA, None).unwrap();
        assert_eq!(zone, DropZone::Bottom);
    }

    #[test]
    fn the_previewed_zone_sticks_near_the_centre() {
        let mut t = Tree::new();
        t.split(0, Orientation::Horizontal);
        // Just left of the middle of pane 0 (0..50 x 0..100).
        let point = (24.0, 50.0);

        let (_, fresh, _) = t.drop_target(point.0, point.1, AREA, None).unwrap();
        assert_eq!(
            fresh,
            DropZone::Left,
            "with nothing on screen, nearest wins"
        );

        let (_, held, _) = t
            .drop_target(point.0, point.1, AREA, Some((0, DropZone::Right)))
            .unwrap();
        assert_eq!(
            held,
            DropZone::Right,
            "a hair past the middle must not flip it"
        );

        let (_, moved, _) = t
            .drop_target(10.0, 50.0, AREA, Some((0, DropZone::Right)))
            .unwrap();
        assert_eq!(moved, DropZone::Left, "a deliberate move still flips it");
    }

    #[test]
    fn the_zone_does_not_stick_across_panes() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        // Held zone belongs to the other pane, so it carries no weight here.
        let (pane, zone, _) = t
            .drop_target(24.0, 50.0, AREA, Some((b, DropZone::Right)))
            .unwrap();
        assert_eq!(pane, 0);
        assert_eq!(zone, DropZone::Left);
    }

    #[test]
    fn the_preview_is_the_half_it_would_take() {
        let mut t = Tree::new();
        t.split(0, Orientation::Horizontal);
        let (_, _, preview) = t.drop_target(45.0, 50.0, AREA, None).unwrap();
        // Right half of the left pane.
        assert_close(preview.x, 25.0);
        assert_close(preview.w, 25.0);
        assert_close(preview.h, 100.0);
    }

    #[test]
    fn rf_pan_05_moving_a_pane_keeps_every_pane() {
        //   +----+----+          +----+----+
        //   |    | b  |          | c  | b  |
        //   | 0  +----+  c left  | 0  +----+
        //   |    | c  |          |    |    |
        //   +----+----+          +----+----+
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();

        assert!(t.move_pane(c, 0, DropZone::Left));
        let mut panes = t.panes();
        panes.sort_unstable();
        assert_eq!(panes, vec![0, b, c], "no pane may be lost or duplicated");

        let l = t.layout(AREA);
        // c ended up left of 0, and b takes the whole right side.
        assert!(l[&c].x < l[&0].x, "c should sit left of 0");
        assert_close(l[&b].h, 100.0);
    }

    #[test]
    fn moving_below_a_pane_stacks_them() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        assert!(t.move_pane(b, 0, DropZone::Bottom));

        let l = t.layout(AREA);
        assert_close(l[&0].w, 100.0);
        assert_close(l[&b].w, 100.0);
        assert!(l[&b].y > l[&0].y, "b should sit below");
    }

    #[test]
    fn a_pane_cannot_be_moved_onto_itself() {
        let mut t = Tree::new();
        t.split(0, Orientation::Horizontal);
        assert!(!t.move_pane(0, 0, DropZone::Left));
    }

    #[test]
    fn moving_leaves_no_gaps_or_overlap() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();
        let d = t.split(c, Orientation::Horizontal).unwrap();

        assert!(t.move_pane(d, 0, DropZone::Top));

        let l = t.layout(AREA);
        assert_eq!(l.len(), 4);
        let total: f32 = l.values().map(|r| r.w * r.h).sum();
        assert_close(total, AREA.w * AREA.h);
    }

    #[test]
    fn navigation_still_works_after_a_move() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();
        t.move_pane(c, 0, DropZone::Bottom);

        for p in t.panes() {
            for d in [
                Direction::Left,
                Direction::Right,
                Direction::Up,
                Direction::Down,
            ] {
                if let Some(n) = t.neighbour(p, d, AREA) {
                    assert_ne!(n, p);
                    assert!(t.panes().contains(&n));
                }
            }
        }
    }

    // --- dividers: the boundaries the user drags ---

    #[test]
    fn a_single_pane_has_no_dividers() {
        let t = Tree::new();
        assert!(t.dividers(AREA, 6.0).is_empty());
    }

    #[test]
    fn one_split_makes_one_divider_on_the_boundary() {
        let mut t = Tree::new();
        t.split(0, Orientation::Horizontal);
        let d = t.dividers(AREA, 6.0);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].orientation, Orientation::Horizontal);
        // Centred on the 50/50 boundary and spanning the full height.
        assert_close(d[0].rect.x + d[0].rect.w / 2.0, 50.0);
        assert_close(d[0].rect.w, 6.0);
        assert_close(d[0].rect.h, 100.0);
    }

    #[test]
    fn nested_splits_expose_one_divider_per_gap() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();
        t.split(c, Orientation::Vertical);
        // One gap on the outer branch, two on the inner one.
        assert_eq!(t.dividers(AREA, 6.0).len(), 3);
    }

    #[test]
    fn divider_at_finds_the_boundary_and_misses_elsewhere() {
        let mut t = Tree::new();
        t.split(0, Orientation::Horizontal);
        assert!(t.divider_at(50.0, 40.0, AREA, 6.0).is_some());
        assert!(t.divider_at(52.0, 40.0, AREA, 6.0).is_some());
        assert!(t.divider_at(20.0, 40.0, AREA, 6.0).is_none());
        assert!(t.divider_at(80.0, 40.0, AREA, 6.0).is_none());
    }

    #[test]
    fn rf_pan_02_dragging_moves_the_boundary_by_the_pixels_asked() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let id = t.dividers(AREA, 6.0)[0].id;

        assert!(t.drag_divider(id, 20.0, AREA));
        let l = t.layout(AREA);
        assert_close(l[&0].w, 70.0);
        assert_close(l[&b].w, 30.0);
    }

    #[test]
    fn rf_pan_02_dragging_only_touches_the_two_neighbours() {
        // Three side by side. Moving the first boundary must leave the third alone,
        // which is what tells a divider drag apart from `resize`.
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Horizontal).unwrap();
        let before = t.layout(AREA)[&c];

        let mut dividers = t.dividers(AREA, 6.0);
        dividers.sort_by(|a, b| a.rect.x.partial_cmp(&b.rect.x).unwrap());
        assert!(t.drag_divider(dividers[0].id, 10.0, AREA));

        let l = t.layout(AREA);
        assert_close(l[&c].w, before.w);
        assert_close(l[&c].x, before.x);
        assert_close(l[&0].w + l[&b].w + l[&c].w, 100.0);
    }

    #[test]
    fn dragging_stops_before_starving_a_neighbour() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let id = t.dividers(AREA, 6.0)[0].id;

        t.drag_divider(id, 10_000.0, AREA);
        let l = t.layout(AREA);
        assert!(l[&b].w >= 4.0, "the neighbour ended up at {}", l[&b].w);
        assert_close(l[&0].w + l[&b].w, 100.0);
    }

    #[test]
    fn dragging_back_returns_to_the_start() {
        let mut t = Tree::new();
        t.split(0, Orientation::Vertical);
        let id = t.dividers(AREA, 6.0)[0].id;

        t.drag_divider(id, 25.0, AREA);
        t.drag_divider(id, -25.0, AREA);
        assert_close(t.layout(AREA)[&0].h, 50.0);
    }

    #[test]
    fn dividers_follow_the_area_they_are_asked_about() {
        let mut t = Tree::new();
        t.split(0, Orientation::Horizontal);
        let wide = Rect::new(0.0, 0.0, 400.0, 200.0);
        let d = t.dividers(wide, 8.0);
        assert_close(d[0].rect.x + d[0].rect.w / 2.0, 200.0);
        assert_close(d[0].rect.h, 200.0);
    }

    // --- directional navigation: the part Warp flags as hard ---

    #[test]
    fn rf_pan_03_moves_between_side_by_side_panes() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        assert_eq!(t.neighbour(0, Direction::Right, AREA), Some(b));
        assert_eq!(t.neighbour(b, Direction::Left, AREA), Some(0));
        assert_eq!(t.neighbour(0, Direction::Left, AREA), None);
        assert_eq!(t.neighbour(0, Direction::Up, AREA), None);
    }

    #[test]
    fn moves_between_stacked_panes() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Vertical).unwrap();
        assert_eq!(t.neighbour(0, Direction::Down, AREA), Some(b));
        assert_eq!(t.neighbour(b, Direction::Up, AREA), Some(0));
    }

    #[test]
    fn rf_pan_03_crosses_branches_by_geometry_not_by_tree_order() {
        // Left spans the full height; the right side is split in two.
        //   +--------+--------+
        //   |        |   b    |
        //   |   0    +--------+
        //   |        |   c    |
        //   +--------+--------+
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();

        // From the top right, moving left lands on 0 even though the tree does
        // not make them siblings.
        assert_eq!(t.neighbour(b, Direction::Left, AREA), Some(0));
        assert_eq!(t.neighbour(c, Direction::Left, AREA), Some(0));
        assert_eq!(t.neighbour(b, Direction::Down, AREA), Some(c));
        assert_eq!(t.neighbour(c, Direction::Up, AREA), Some(b));
    }

    #[test]
    fn picks_the_neighbour_that_overlaps_most() {
        // 0 spans the left. Three panes are stacked on the right; the one that
        // overlaps 0 most wins, ties going to the nearest.
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();
        let d = t.split(c, Orientation::Vertical).unwrap();

        let l = t.layout(AREA);
        assert_close(l[&b].h, 50.0);
        assert_close(l[&c].h, 25.0);
        assert_close(l[&d].h, 25.0);

        // b is the tallest, so it overlaps 0 most.
        assert_eq!(t.neighbour(0, Direction::Right, AREA), Some(b));
    }

    #[test]
    fn never_returns_a_pane_that_only_touches_at_a_corner() {
        //   +--------+--------+
        //   |   0    |   b    |
        //   +--------+--------+
        //   |   d    |   c    |
        //   +--------+--------+
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();
        let d = t.split(0, Orientation::Vertical).unwrap();

        // No diagonal neighbour: 0 and c only touch at the corner.
        assert_ne!(t.neighbour(0, Direction::Down, AREA), Some(c));
        assert_eq!(t.neighbour(0, Direction::Down, AREA), Some(d));
        assert_eq!(t.neighbour(0, Direction::Right, AREA), Some(b));
    }

    #[test]
    fn navigation_survives_closing_panes() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();
        t.close(b);
        assert_eq!(t.neighbour(0, Direction::Right, AREA), Some(c));
        assert_eq!(t.neighbour(c, Direction::Left, AREA), Some(0));
    }

    #[test]
    fn ids_are_reused_without_confusing_the_tree() {
        let mut t = Tree::new();
        for _ in 0..20 {
            let p = t.split(0, Orientation::Horizontal).unwrap();
            assert!(t.close(p));
        }
        assert_eq!(t.panes(), vec![0]);
        assert_eq!(t.layout(AREA)[&0], AREA);
    }

    // --- external input that must never panic or corrupt the tree ---

    #[test]
    fn resize_in_a_crowded_branch_does_not_panic() {
        // Past 20 siblings the minimum shares no longer fit, which used to make
        // f32::clamp panic.
        let mut t = Tree::new();
        for _ in 0..24 {
            t.split(0, Orientation::Horizontal).unwrap();
        }
        assert_eq!(t.len(), 25);
        assert!(!t.resize(0, 0.1));
        let total: f32 = t.layout(AREA).values().map(|r| r.w * r.h).sum();
        assert_close(total, AREA.w * AREA.h);
    }

    #[test]
    fn non_finite_deltas_are_refused() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let id = t.dividers(AREA, 6.0)[0].id;
        for delta in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(!t.resize(0, delta));
            assert!(!t.drag_divider(id, delta, AREA));
        }
        let l = t.layout(AREA);
        assert_close(l[&0].w, 50.0);
        assert_close(l[&b].w, 50.0);
    }

    #[test]
    fn a_stale_divider_id_is_refused_after_a_close() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let c = t.split(b, Orientation::Vertical).unwrap();
        let inner = t
            .dividers(AREA, 6.0)
            .into_iter()
            .find(|d| d.orientation == Orientation::Vertical)
            .unwrap()
            .id;
        let outer = t
            .dividers(AREA, 6.0)
            .into_iter()
            .find(|d| d.orientation == Orientation::Horizontal)
            .unwrap()
            .id;

        assert!(t.close(c));
        assert!(!t.drag_divider(inner, 10.0, AREA));
        assert!(t.close(b));
        assert!(!t.drag_divider(outer, 10.0, AREA));
        assert_eq!(t.layout(AREA)[&0], AREA);
    }

    #[test]
    fn non_finite_areas_are_refused() {
        let mut t = Tree::new();
        let b = t.split(0, Orientation::Horizontal).unwrap();
        let id = t.dividers(AREA, 6.0)[0].id;
        for area in [
            Rect::new(0.0, 0.0, f32::NAN, 100.0),
            Rect::new(0.0, 0.0, f32::INFINITY, 100.0),
        ] {
            assert!(!t.drag_divider(id, 10.0, area));
        }
        assert!(t.drag_divider(id, 10.0, AREA));
        let l = t.layout(AREA);
        assert!(l.values().all(|r| r.w.is_finite()));
        assert_close(l[&0].w, 60.0);
        assert_close(l[&b].w, 40.0);
    }

    #[test]
    fn dragging_between_squeezed_panes_never_inverts_them() {
        // Each split halves the last pane, so the final two end up far below
        // the minimum share and there is no room left to move their boundary.
        let mut t = Tree::new();
        let mut last = 0;
        for _ in 0..6 {
            last = t.split(last, Orientation::Horizontal).unwrap();
        }
        let mut dividers = t.dividers(AREA, 1.0);
        dividers.sort_by(|a, b| a.rect.x.partial_cmp(&b.rect.x).unwrap());
        let squeezed = dividers.last().unwrap().id;

        for delta in [1000.0, -1000.0] {
            t.drag_divider(squeezed, delta, AREA);
            for (pane, r) in t.layout(AREA) {
                assert!(r.w > 0.0, "pane {pane} has width {}", r.w);
            }
        }
    }

    #[test]
    fn from_shape_rebuilds_structure_ratios_and_ids() {
        let mut tree = Tree::new();
        let right = tree.split(0, Orientation::Horizontal).unwrap();
        let below = tree.split(right, Orientation::Vertical).unwrap();
        tree.split_towards(0, DropZone::Top).unwrap();
        assert!(tree.resize(below, 0.15));
        let copy = Tree::from_shape(&tree.shape()).unwrap();
        assert_eq!(copy.shape(), tree.shape());
        assert_eq!(copy.layout(AREA), tree.layout(AREA));
        let mut copy = copy;
        let next = copy.split(below, Orientation::Horizontal).unwrap();
        assert!(!tree.panes().contains(&next), "a new pane reuses an id");
        assert!(copy.close(next));
    }

    #[test]
    fn from_shape_scales_drifted_ratios_and_refuses_broken_shapes() {
        let leaf = Shape::Pane;
        let split = |children| Shape::Split {
            orientation: Orientation::Horizontal,
            children,
        };
        let drifted = Tree::from_shape(&split(vec![(1.0, leaf(4)), (3.0, leaf(9))])).unwrap();
        let Shape::Split { children, .. } = drifted.shape() else {
            panic!("expected a split");
        };
        assert_close(children[0].0, 0.25);
        assert_close(children[1].0, 0.75);
        assert_eq!(Tree::from_shape(&leaf(7)).unwrap().panes(), vec![7]);
        for broken in [
            split(vec![(1.0, leaf(1))]),
            split(vec![(0.5, leaf(1)), (0.5, leaf(1))]),
            split(vec![(0.0, leaf(1)), (1.0, leaf(2))]),
            split(vec![(f32::NAN, leaf(1)), (0.5, leaf(2))]),
            split(vec![(0.5, leaf(1)), (0.5, split(Vec::new()))]),
            leaf(PaneId::MAX),
        ] {
            assert!(Tree::from_shape(&broken).is_none(), "{broken:?}");
        }
    }
}

#[cfg(test)]
mod fuzz {
    use super::*;

    /// Deterministic so a failure is reproducible; no dependency for it.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn check_invariants(t: &Tree, area: Rect) {
        let panes = t.panes();
        assert!(!panes.is_empty(), "the tree ran out of panes");

        let mut sorted = panes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), panes.len(), "duplicated panes: {panes:?}");

        let rects = t.layout(area);
        assert_eq!(rects.len(), panes.len(), "layout and tree disagree");

        let total: f32 = rects.values().map(|r| r.w * r.h).sum();
        assert!(
            (total - area.w * area.h).abs() < 0.5,
            "the area is not covered: {total} vs {}",
            area.w * area.h
        );

        for r in rects.values() {
            assert!(r.w > 0.0 && r.h > 0.0, "degenerate rectangle: {r:?}");
        }

        let list: Vec<&Rect> = rects.values().collect();
        for (i, a) in list.iter().enumerate() {
            for b in &list[i + 1..] {
                let overlap = (a.right().min(b.right()) - a.x.max(b.x)).max(0.0)
                    * (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0.0);
                assert!(overlap < 0.5, "overlapping: {a:?} and {b:?}");
            }
        }
    }

    #[test]
    fn random_operations_keep_the_tree_sane() {
        let area = Rect::new(0.0, 0.0, 1000.0, 800.0);
        for seed in 0..40u64 {
            let mut rng = Lcg(seed.wrapping_mul(0x9E3779B9) + 1);
            let mut t = Tree::new();

            for _ in 0..120 {
                let panes = t.panes();
                let pane = panes[rng.below(panes.len())];
                match rng.below(14) {
                    0..=3 => {
                        let o = if rng.below(2) == 0 {
                            Orientation::Horizontal
                        } else {
                            Orientation::Vertical
                        };
                        t.split(pane, o);
                    }
                    4 => {
                        let zones = [
                            DropZone::Left,
                            DropZone::Right,
                            DropZone::Top,
                            DropZone::Bottom,
                        ];
                        t.split_towards(pane, zones[rng.below(4)]);
                    }
                    5..=7 => {
                        t.close(pane);
                    }
                    8 | 9 => {
                        let delta = (rng.below(21) as f32 - 10.0) / 50.0;
                        t.resize(pane, delta);
                    }
                    10 | 11 => {
                        let target = panes[rng.below(panes.len())];
                        let zones = [
                            DropZone::Left,
                            DropZone::Right,
                            DropZone::Top,
                            DropZone::Bottom,
                        ];
                        t.move_pane(pane, target, zones[rng.below(4)]);
                    }
                    _ => {
                        let dividers = t.dividers(area, 6.0);
                        if !dividers.is_empty() {
                            let id = dividers[rng.below(dividers.len())].id;
                            let delta = rng.below(801) as f32 - 400.0;
                            t.drag_divider(id, delta, area);
                        }
                    }
                }
                check_invariants(&t, area);
            }

            // Navigation must never return a missing pane, nor the pane itself.
            let panes = t.panes();
            for &p in &panes {
                for d in [
                    Direction::Left,
                    Direction::Right,
                    Direction::Up,
                    Direction::Down,
                ] {
                    if let Some(n) = t.neighbour(p, d, area) {
                        assert_ne!(n, p, "navigated back to itself");
                        assert!(panes.contains(&n), "no such neighbour: {n}");
                    }
                }
            }
        }
    }

    #[test]
    fn moving_and_coming_back_is_stable() {
        let area = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let mut rng = Lcg(7);
        let mut t = Tree::new();
        for _ in 0..30 {
            let panes = t.panes();
            let pane = panes[rng.below(panes.len())];
            let o = if rng.below(2) == 0 {
                Orientation::Horizontal
            } else {
                Orientation::Vertical
            };
            t.split(pane, o);
        }

        // Going and coming back must land somewhere that leads back: otherwise
        // navigation "gets lost" and the user notices.
        for &p in &t.panes() {
            for (go, back) in [
                (Direction::Left, Direction::Right),
                (Direction::Up, Direction::Down),
            ] {
                if let Some(n) = t.neighbour(p, go, area) {
                    let there_and_back = t.neighbour(n, back, area);
                    assert!(
                        there_and_back.is_some(),
                        "from {p} going {go:?} reaches {n}, but {n} does not lead back"
                    );
                }
            }
        }
    }
}
