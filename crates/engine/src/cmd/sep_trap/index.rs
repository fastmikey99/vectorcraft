//! Balanced segment bounds tree. Exact cubic distances preserve the linear search's
//! ownership and tie order, while pruning distant portions of large compound paths.
use super::*;
use kurbo::{CubicBez, ParamCurveExtrema, ParamCurveNearest};
use vectorcraft_geom::Point;

struct Segment {
    curve: CubicBez,
    bounds: Rect,
    owner: usize,
    order: usize,
}
struct Branch {
    bounds: Rect,
    range: std::ops::Range<usize>,
    children: Option<(usize, usize)>,
}
pub(super) struct Index {
    segments: Vec<Segment>,
    branches: Vec<Branch>,
}
fn distance(b: Rect, p: Point) -> f64 {
    let x = (b.x0 - p.x).max(0.0).max(p.x - b.x1);
    let y = (b.y0 - p.y).max(0.0).max(p.y - b.y1);
    x * x + y * y
}
impl Index {
    pub(super) fn new(paths: impl Iterator<Item = (usize, PathData)>) -> Self {
        let mut segments = Vec::new();
        for (owner, path) in paths {
            for sub in &path.subpaths {
                for i in 0..sub.segment_count() {
                    let curve = sub.segment(i);
                    segments.push(Segment { curve, bounds: curve.bounding_box(), owner, order: segments.len() });
                }
            }
        }
        let mut index = Self { segments, branches: Vec::new() };
        if !index.segments.is_empty() {
            index.build(0..index.segments.len());
        }
        index
    }
    fn build(&mut self, range: std::ops::Range<usize>) -> usize {
        let slice = self.segments.get(range.clone()).unwrap_or_default();
        let bounds = slice.iter().map(|s| s.bounds).reduce(|a, b| a.union(b)).unwrap_or(Rect::ZERO);
        let at = self.branches.len();
        self.branches.push(Branch { bounds, range: range.clone(), children: None });
        if range.len() > 8 {
            let horizontal = bounds.width() >= bounds.height();
            if let Some(slice) = self.segments.get_mut(range.clone()) {
                slice.sort_unstable_by(|a, b| {
                    let center = |s: &Segment| if horizontal { s.bounds.center().x } else { s.bounds.center().y };
                    center(a).total_cmp(&center(b)).then(a.order.cmp(&b.order))
                });
            }
            let mid = range.start + range.len() / 2;
            let left = self.build(range.start..mid);
            let right = self.build(mid..range.end);
            if let Some(branch) = self.branches.get_mut(at) {
                branch.children = Some((left, right));
            }
        }
        at
    }
    pub(super) fn nearest(&self, p: Point, allowed: impl Fn(usize) -> bool) -> Option<usize> {
        let mut best = (f64::INFINITY, usize::MAX, None);
        self.search(0, p, &allowed, &mut best);
        best.2
    }
    fn search(&self, at: usize, p: Point, allowed: &impl Fn(usize) -> bool, best: &mut (f64, usize, Option<usize>)) {
        let Some(branch) = self.branches.get(at) else { return };
        if distance(branch.bounds, p) > best.0 * best.0 + 1e-12 {
            return;
        }
        if let Some((a, b)) = branch.children {
            let da = self.branches.get(a).map_or(f64::INFINITY, |b| distance(b.bounds, p));
            let db = self.branches.get(b).map_or(f64::INFINITY, |b| distance(b.bounds, p));
            let (a, b) = if da <= db { (a, b) } else { (b, a) };
            self.search(a, p, allowed, best);
            self.search(b, p, allowed, best);
        } else {
            for s in self.segments.get(branch.range.clone()).unwrap_or_default() {
                if allowed(s.owner) && distance(s.bounds, p) <= best.0 * best.0 + 1e-12 {
                    let d = s.curve.nearest(p, 1e-6).distance_sq.sqrt();
                    if d < best.0 || (d == best.0 && s.order < best.1) {
                        *best = (d, s.order, Some(s.owner));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_owners_equal_linear_search_including_ties_and_exclusions() {
        let paths: Vec<_> = (0..25)
            .map(|i| {
                let x = f64::from(i % 5) * 4.;
                let y = f64::from(i / 5) * 4.;
                vectorcraft_geom::shapes::ellipse(Rect::new(x, y, x + 3., y + 3.))
            })
            .collect();
        let index = Index::new(paths.iter().cloned().enumerate());
        for y in 0..51 {
            for x in 0..51 {
                let p = Point::new(f64::from(x) * 0.4 - 0.5, f64::from(y) * 0.4 - 0.5);
                let allowed = |i: usize| !i.is_multiple_of(3);
                let linear = paths
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| allowed(*i))
                    .filter_map(|(i, path)| path.nearest(p).map(|v| (i, v.4)))
                    .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
                    .map(|v| v.0);
                assert_eq!(index.nearest(p, allowed), linear, "{p:?}");
            }
        }
    }
}
