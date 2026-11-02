//! N-ary split layout tree for panes (pure logic, no Win32).

use serde::{Deserialize, Serialize};

pub type PaneId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w: w.max(0), h: h.max(0) }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Layout {
    Leaf(PaneId),
    /// `horizontal`: children are laid out left-to-right (vertical splitters).
    Split { horizontal: bool, children: Vec<Layout>, weights: Vec<f64> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Splitter {
    /// Child-index path from the root to the split node.
    pub path: Vec<usize>,
    /// Splitter sits between child `index` and `index + 1`.
    pub index: usize,
    pub horizontal: bool,
    pub rect: Rect,
    /// Start (x or y) of child `index`.
    pub span_start: i32,
    /// Combined size of children `index` and `index + 1` (excluding the splitter).
    pub span_len: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Layout {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Layout::Leaf(p) => out.push(*p),
            Layout::Split { children, .. } => children.iter().for_each(|c| c.collect(out)),
        }
    }

    /// Places `new` next to `target` on `side`.
    pub fn split(&mut self, target: PaneId, new: PaneId, side: Side) -> bool {
        let horizontal = matches!(side, Side::Left | Side::Right);
        let after = matches!(side, Side::Right | Side::Bottom);
        self.split_inner(target, new, horizontal, after)
    }

    fn split_inner(&mut self, target: PaneId, new: PaneId, horizontal: bool, after: bool) -> bool {
        match self {
            Layout::Leaf(p) if *p == target => {
                let children = if after {
                    vec![Layout::Leaf(target), Layout::Leaf(new)]
                } else {
                    vec![Layout::Leaf(new), Layout::Leaf(target)]
                };
                *self = Layout::Split { horizontal, children, weights: vec![1.0, 1.0] };
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split { horizontal: h, children, weights } => {
                // Same orientation: insert as a sibling, halving the target's share.
                if *h == horizontal {
                    if let Some(i) = children.iter().position(|c| *c == Layout::Leaf(target)) {
                        let half = weights[i] / 2.0;
                        weights[i] = half;
                        let at = if after { i + 1 } else { i };
                        children.insert(at, Layout::Leaf(new));
                        weights.insert(at, half);
                        return true;
                    }
                }
                children.iter_mut().any(|c| c.split_inner(target, new, horizontal, after))
            }
        }
    }

    /// Removes a pane; collapses single-child splits and merges same-orientation nesting.
    /// Returns false if the pane is the last one (the root leaf is never removed).
    pub fn remove(&mut self, pane: PaneId) -> bool {
        if let Layout::Leaf(_) = self {
            return false;
        }
        let removed = self.remove_inner(pane);
        self.normalize();
        removed
    }

    fn remove_inner(&mut self, pane: PaneId) -> bool {
        if let Layout::Split { children, weights, .. } = self {
            if let Some(i) = children.iter().position(|c| *c == Layout::Leaf(pane)) {
                children.remove(i);
                weights.remove(i);
                return true;
            }
            return children.iter_mut().any(|c| c.remove_inner(pane));
        }
        false
    }

    fn normalize(&mut self) {
        if let Layout::Split { horizontal, children, weights } = self {
            for c in children.iter_mut() {
                c.normalize();
            }
            // Flatten children that are splits with the same orientation.
            let mut i = 0;
            while i < children.len() {
                let flatten = matches!(&children[i], Layout::Split { horizontal: h, .. } if *h == *horizontal);
                if flatten {
                    if let Layout::Split { children: sub, weights: sw, .. } = children.remove(i) {
                        let w = weights.remove(i);
                        let total: f64 = sw.iter().sum::<f64>().max(f64::EPSILON);
                        let n = sub.len();
                        for (k, (c, cw)) in sub.into_iter().zip(sw).enumerate() {
                            children.insert(i + k, c);
                            weights.insert(i + k, w * cw / total);
                        }
                        i += n;
                        continue;
                    }
                }
                i += 1;
            }
            if children.len() == 1 {
                *self = children.remove(0);
            }
        }
    }

    /// Computes pane rectangles and splitter hit areas.
    pub fn arrange(&self, area: Rect, splitter: i32, out: &mut Vec<(PaneId, Rect)>, splitters: &mut Vec<Splitter>) {
        let mut path = Vec::new();
        self.arrange_inner(area, splitter, &mut path, out, splitters);
    }

    fn arrange_inner(
        &self,
        area: Rect,
        sp: i32,
        path: &mut Vec<usize>,
        out: &mut Vec<(PaneId, Rect)>,
        splitters: &mut Vec<Splitter>,
    ) {
        match self {
            Layout::Leaf(p) => out.push((*p, area)),
            Layout::Split { horizontal, children, weights } => {
                let n = children.len() as i32;
                let len = if *horizontal { area.w } else { area.h };
                let avail = (len - sp * (n - 1)).max(0);
                let total: f64 = weights.iter().sum::<f64>().max(f64::EPSILON);
                let mut sizes: Vec<i32> = weights.iter().map(|w| ((avail as f64) * w / total).floor() as i32).collect();
                let used: i32 = sizes.iter().sum();
                if let Some(last) = sizes.last_mut() {
                    *last += avail - used;
                }
                let mut pos = if *horizontal { area.x } else { area.y };
                for (i, child) in children.iter().enumerate() {
                    let size = sizes[i];
                    let r = if *horizontal {
                        Rect::new(pos, area.y, size, area.h)
                    } else {
                        Rect::new(area.x, pos, area.w, size)
                    };
                    path.push(i);
                    child.arrange_inner(r, sp, path, out, splitters);
                    path.pop();
                    if i + 1 < children.len() {
                        let srect = if *horizontal {
                            Rect::new(pos + size, area.y, sp, area.h)
                        } else {
                            Rect::new(area.x, pos + size, area.w, sp)
                        };
                        splitters.push(Splitter {
                            path: path.clone(),
                            index: i,
                            horizontal: *horizontal,
                            rect: srect,
                            span_start: pos,
                            span_len: size + sizes[i + 1],
                        });
                    }
                    pos += size + sp;
                }
            }
        }
    }

    fn node_at_mut(&mut self, path: &[usize]) -> Option<&mut Layout> {
        let mut node = self;
        for &i in path {
            match node {
                Layout::Split { children, .. } => node = children.get_mut(i)?,
                Layout::Leaf(_) => return None,
            }
        }
        Some(node)
    }

    /// Moves a splitter so child `index` gets `pos - span_start` pixels.
    pub fn drag_splitter(&mut self, s: &Splitter, pos: i32, min_size: i32) {
        if let Some(Layout::Split { weights, .. }) = self.node_at_mut(&s.path) {
            if s.index + 1 >= weights.len() || s.span_len <= 0 {
                return;
            }
            let min = min_size.min(s.span_len / 2);
            let first = (pos - s.span_start).clamp(min, s.span_len - min);
            let combined = weights[s.index] + weights[s.index + 1];
            weights[s.index] = combined * first as f64 / s.span_len as f64;
            weights[s.index + 1] = combined - weights[s.index];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrange(l: &Layout, w: i32, h: i32) -> (Vec<(PaneId, Rect)>, Vec<Splitter>) {
        let mut out = Vec::new();
        let mut sp = Vec::new();
        l.arrange(Rect::new(0, 0, w, h), 4, &mut out, &mut sp);
        (out, sp)
    }

    #[test]
    fn split_and_arrange() {
        let mut l = Layout::Leaf(1);
        assert!(l.split(1, 2, Side::Right));
        assert!(l.split(2, 3, Side::Right));
        assert_eq!(l.panes(), vec![1, 2, 3]);
        // Same orientation inserts as sibling instead of nesting.
        assert!(matches!(&l, Layout::Split { children, .. } if children.len() == 3));
        let (rects, sps) = arrange(&l, 404, 100);
        assert_eq!(rects[0].1, Rect::new(0, 0, 198, 100));
        assert_eq!(rects[1].1.w + rects[2].1.w, 198);
        assert_eq!(sps.len(), 2);
        assert_eq!(rects.iter().map(|r| r.1.w).sum::<i32>() + 8, 404);
    }

    #[test]
    fn split_down_nests() {
        let mut l = Layout::Leaf(1);
        l.split(1, 2, Side::Right);
        l.split(2, 3, Side::Bottom);
        assert_eq!(l.panes(), vec![1, 2, 3]);
        let (rects, _) = arrange(&l, 204, 104);
        let r3 = rects.iter().find(|r| r.0 == 3).unwrap().1;
        assert_eq!(r3.x, 104);
        assert_eq!(r3.y, 54);
        l.split(1, 4, Side::Left);
        assert_eq!(l.panes(), vec![4, 1, 2, 3]);
    }

    #[test]
    fn remove_collapses() {
        let mut l = Layout::Leaf(1);
        l.split(1, 2, Side::Right);
        l.split(2, 3, Side::Bottom);
        assert!(l.remove(3));
        assert_eq!(l, Layout::Split { horizontal: true, children: vec![Layout::Leaf(1), Layout::Leaf(2)], weights: vec![1.0, 1.0] });
        assert!(l.remove(1));
        assert_eq!(l, Layout::Leaf(2));
        assert!(!l.remove(2));
    }

    #[test]
    fn remove_flattens_same_orientation() {
        let mut l = Layout::Leaf(1);
        l.split(1, 2, Side::Right);
        l.split(2, 3, Side::Bottom);
        l.split(3, 4, Side::Right);
        // Removing 2 leaves a vertical split with one child (a horizontal split) which then flattens into the root.
        l.remove(2);
        assert_eq!(l.panes(), vec![1, 3, 4]);
        assert!(matches!(&l, Layout::Split { horizontal: true, children, .. } if children.len() == 3));
    }

    #[test]
    fn drag() {
        let mut l = Layout::Leaf(1);
        l.split(1, 2, Side::Right);
        let (_, sps) = arrange(&l, 404, 100);
        l.drag_splitter(&sps[0], 100, 50);
        let (rects, _) = arrange(&l, 404, 100);
        assert_eq!(rects[0].1.w, 100);
        l.drag_splitter(&sps[0], 10, 50);
        let (rects, _) = arrange(&l, 404, 100);
        assert_eq!(rects[0].1.w, 50);
        l.drag_splitter(&sps[0], 1000, 50);
        let (rects, _) = arrange(&l, 404, 100);
        assert_eq!(rects[1].1.w, 50);
    }
}
