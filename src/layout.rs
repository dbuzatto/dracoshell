//! Binary-space-partitioning tiling tree for terminal panes. Each leaf is a
//! pane identified by [`PaneId`]; internal nodes split a region either
//! horizontally (left | right) or vertically (top / bottom).

pub type PaneId = u64;

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Copy)]
pub enum Split {
    /// Left | Right (vertical divider).
    Horizontal,
    /// Top / Bottom (horizontal divider).
    Vertical,
}

#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

pub enum Node {
    Leaf(PaneId),
    Branch {
        split: Split,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

pub struct Layout {
    root: Node,
    focused: PaneId,
}

impl Layout {
    pub fn new(initial: PaneId) -> Self {
        Self {
            root: Node::Leaf(initial),
            focused: initial,
        }
    }

    pub fn focused(&self) -> PaneId {
        self.focused
    }

    pub fn set_focused(&mut self, id: PaneId) {
        self.focused = id;
    }

    /// Compute pixel rectangles for every leaf inside `viewport`.
    pub fn compute(&self, viewport: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        compute(&self.root, viewport, &mut out);
        out
    }

    /// Replace the focused leaf with a branch holding the original leaf and
    /// a fresh leaf for `new_pane`. Focus moves to the new pane.
    pub fn split_focused(&mut self, new_pane: PaneId, split: Split) {
        let focused = self.focused;
        split_walk(&mut self.root, focused, new_pane, split);
        self.focused = new_pane;
    }

    /// Remove the focused leaf, collapsing its parent into the surviving
    /// sibling. Returns `false` if there is only one pane left (refused).
    pub fn close_focused(&mut self) -> bool {
        if matches!(self.root, Node::Leaf(id) if id == self.focused) {
            return false;
        }
        remove_walk(&mut self.root, self.focused);
        self.focused = leftmost_leaf(&self.root);
        true
    }

    /// Move focus to the geometrically nearest leaf in `dir`.
    pub fn focus_neighbor(&mut self, dir: Direction, viewport: Rect) {
        let layout = self.compute(viewport);
        let Some(from) = layout
            .iter()
            .find(|(id, _)| *id == self.focused)
            .map(|(_, r)| *r)
        else {
            return;
        };
        let (fx, fy) = (from.x + from.w / 2.0, from.y + from.h / 2.0);
        let best = layout
            .iter()
            .filter(|(id, _)| *id != self.focused)
            .filter_map(|(id, r)| {
                let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                let dx = cx - fx;
                let dy = cy - fy;
                let in_dir = match dir {
                    Direction::Left => dx < 0.0 && dx.abs() >= dy.abs(),
                    Direction::Right => dx > 0.0 && dx.abs() >= dy.abs(),
                    Direction::Up => dy < 0.0 && dy.abs() >= dx.abs(),
                    Direction::Down => dy > 0.0 && dy.abs() >= dx.abs(),
                };
                in_dir.then_some((*id, dx * dx + dy * dy))
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        if let Some((id, _)) = best {
            self.focused = id;
        }
    }

    #[allow(dead_code)]
    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        collect_leaves(&self.root, &mut out);
        out
    }
}

fn compute(node: &Node, r: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match node {
        Node::Leaf(id) => out.push((*id, r)),
        Node::Branch { split, ratio, a, b } => {
            let (ra, rb) = match split {
                Split::Horizontal => {
                    let wa = r.w * ratio;
                    (
                        Rect {
                            x: r.x,
                            y: r.y,
                            w: wa,
                            h: r.h,
                        },
                        Rect {
                            x: r.x + wa,
                            y: r.y,
                            w: r.w - wa,
                            h: r.h,
                        },
                    )
                }
                Split::Vertical => {
                    let ha = r.h * ratio;
                    (
                        Rect {
                            x: r.x,
                            y: r.y,
                            w: r.w,
                            h: ha,
                        },
                        Rect {
                            x: r.x,
                            y: r.y + ha,
                            w: r.w,
                            h: r.h - ha,
                        },
                    )
                }
            };
            compute(a, ra, out);
            compute(b, rb, out);
        }
    }
}

fn split_walk(node: &mut Node, target: PaneId, new_pane: PaneId, split: Split) -> bool {
    match node {
        Node::Leaf(id) if *id == target => {
            let old_id = *id;
            *node = Node::Branch {
                split,
                ratio: 0.5,
                a: Box::new(Node::Leaf(old_id)),
                b: Box::new(Node::Leaf(new_pane)),
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Branch { a, b, .. } => {
            split_walk(a, target, new_pane, split) || split_walk(b, target, new_pane, split)
        }
    }
}

fn remove_walk(parent: &mut Node, target: PaneId) -> bool {
    if let Node::Branch { a, b, .. } = parent {
        if let Node::Leaf(id) = **a {
            if id == target {
                let new = std::mem::replace(b.as_mut(), Node::Leaf(0));
                *parent = new;
                return true;
            }
        }
        if let Node::Leaf(id) = **b {
            if id == target {
                let new = std::mem::replace(a.as_mut(), Node::Leaf(0));
                *parent = new;
                return true;
            }
        }
        if remove_walk(a, target) {
            return true;
        }
        if remove_walk(b, target) {
            return true;
        }
    }
    false
}

fn leftmost_leaf(node: &Node) -> PaneId {
    match node {
        Node::Leaf(id) => *id,
        Node::Branch { a, .. } => leftmost_leaf(a),
    }
}

#[allow(dead_code)]
fn collect_leaves(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Branch { a, b, .. } => {
            collect_leaves(a, out);
            collect_leaves(b, out);
        }
    }
}
