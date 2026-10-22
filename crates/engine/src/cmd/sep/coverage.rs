//! Conservative coverage proofs, independent of the cleaned/refitted production geometry.
//! A control-point box encloses every Bezier segment. Rectangles provide exact lower
//! coverage; other shapes remain unknown. CSG operations retain uncertainty instead of
//! deleting thin contours. A proof can decline valid coverage, but cannot fill a hole.
use vectorcraft_geom::{PathData, Rect};

#[derive(Clone, Default)]
pub(super) enum Region {
    #[default]
    Empty,
    Shape {
        bounds: Rect,
        rectangle: bool,
    },
    Op {
        a: Box<Region>,
        b: Box<Region>,
        kind: Kind,
        bounds: Rect,
        depth: u8,
    },
}
#[derive(Clone, Copy)]
pub(super) enum Kind {
    Union,
    Intersect,
    Difference,
}

pub(super) fn control_bounds(path: &PathData) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for a in path.subpaths.iter().flat_map(|s| &s.anchors) {
        for p in [a.p, a.h_in, a.h_out] {
            if !p.x.is_finite() || !p.y.is_finite() {
                return None;
            }
            bounds = Some(bounds.map_or(Rect::new(p.x, p.y, p.x, p.y), |r| r.union_pt(p)));
        }
    }
    bounds
}
// Also recognize straight cubic rectangle edges: mask meshes often retain
// collinear handles. Exact axis comparisons avoid treating a near-line as straight.
fn rectangle(path: &PathData) -> Option<Rect> {
    let [sp] = path.subpaths.as_slice() else { return None };
    if !sp.closed || sp.anchors.len() != 4 {
        return None;
    }
    let bounds = control_bounds(path)?;
    for (a, b) in sp.anchors.iter().zip(sp.anchors.iter().cycle().skip(1)).take(4) {
        if a.p.x == b.p.x && a.p.y != b.p.y {
            if a.h_out.x != a.p.x
                || b.h_in.x != a.p.x
                || a.h_out.y < a.p.y.min(b.p.y)
                || a.h_out.y > a.p.y.max(b.p.y)
                || b.h_in.y < a.p.y.min(b.p.y)
                || b.h_in.y > a.p.y.max(b.p.y)
            {
                return None;
            }
        } else if a.p.y == b.p.y && a.p.x != b.p.x {
            if a.h_out.y != a.p.y
                || b.h_in.y != a.p.y
                || a.h_out.x < a.p.x.min(b.p.x)
                || a.h_out.x > a.p.x.max(b.p.x)
                || b.h_in.x < a.p.x.min(b.p.x)
                || b.h_in.x > a.p.x.max(b.p.x)
            {
                return None;
            }
        } else {
            return None;
        }
    }
    let mut corners = sp.anchors.iter().map(|a| (a.p.x.to_bits(), a.p.y.to_bits())).collect::<std::collections::BTreeSet<_>>();
    for (x, y) in [(bounds.x0, bounds.y0), (bounds.x1, bounds.y0), (bounds.x1, bounds.y1), (bounds.x0, bounds.y1)] {
        if !corners.remove(&(x.to_bits(), y.to_bits())) {
            return None;
        }
    }
    Some(bounds)
}

// A Coons patch is the sum of two interpolated boundary curves minus its
// bilinear corners. If all boundary controls lie in [lo, hi], every patch point
// lies in [2*lo-hi, 2*hi-lo]. Include roundoff outward; do not assume the outline
// contains an internally folded patch.
pub(super) fn mesh_bounds(mesh: &vectorcraft_doc::GradientMesh) -> Option<Rect> {
    let bounds = mesh.bounds()?;
    if mesh.points.iter().any(|p| !p.p.x.is_finite() || !p.p.y.is_finite() || p.handles.iter().any(|h| !h.x.is_finite() || !h.y.is_finite())) {
        return None;
    }
    let scale = [bounds.x0, bounds.x1, bounds.y0, bounds.y1].into_iter().map(f64::abs).fold(1.0, f64::max);
    let margin = scale * f64::EPSILON * 64.0;
    Some(bounds.inflate(bounds.width() + margin, bounds.height() + margin))
}

impl Region {
    pub(super) fn path(path: &PathData) -> Self {
        match control_bounds(path) {
            Some(bounds) => Self::Shape { bounds, rectangle: rectangle(path).is_some() },
            None => Self::Shape { bounds: Rect::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::INFINITY), rectangle: false },
        }
    }
    pub(super) fn unknown(bounds: Rect) -> Self {
        Self::Shape { bounds, rectangle: false }
    }
    pub(super) fn rect(bounds: Rect) -> Self {
        Self::Shape { bounds, rectangle: true }
    }
    pub(super) fn bounds(&self) -> Option<Rect> {
        match self {
            Self::Empty => None,
            Self::Shape { bounds, .. } | Self::Op { bounds, .. } => Some(*bounds),
        }
    }
    fn depth(&self) -> u8 {
        match self {
            Self::Op { depth, .. } => *depth,
            _ => 0,
        }
    }
    pub(super) fn disjoint(&self, target: Rect) -> bool {
        self.bounds().is_none_or(|r| r.x1 <= target.x0 || target.x1 <= r.x0 || r.y1 <= target.y0 || target.y1 <= r.y0)
    }
    pub(super) fn contains(&self, target: Rect) -> bool {
        if ![target.x0, target.y0, target.x1, target.y1].iter().all(|v| v.is_finite()) {
            return false;
        }
        if target.x1 <= target.x0 || target.y1 <= target.y0 {
            return true;
        }
        match self {
            Self::Empty => false,
            Self::Shape { bounds, rectangle } => *rectangle && bounds.contains_rect(target),
            Self::Op { a, b, kind, .. } => match kind {
                Kind::Union => a.contains(target) || b.contains(target),
                Kind::Intersect => a.contains(target) && b.contains(target),
                Kind::Difference => a.contains(target) && b.disjoint(target),
            },
        }
    }
    pub(super) fn combine(self, other: Self, kind: Kind) -> Self {
        let (Some(a), Some(b)) = (self.bounds(), other.bounds()) else {
            return match kind {
                Kind::Union => {
                    if self.bounds().is_some() {
                        self
                    } else {
                        other
                    }
                }
                Kind::Difference => self,
                Kind::Intersect => Self::Empty,
            };
        };
        if matches!(kind, Kind::Union) {
            if self.contains(b) {
                return self;
            }
            if other.contains(a) {
                return other;
            }
        }
        let bounds = match kind {
            Kind::Union => a.union(b),
            Kind::Difference => a,
            Kind::Intersect => a.intersect(b),
        };
        if bounds.x1 <= bounds.x0 || bounds.y1 <= bounds.y0 {
            return Self::Empty;
        }
        let depth = 1 + self.depth().max(other.depth());
        if depth > 32 {
            return Self::Shape { bounds, rectangle: false };
        }
        Self::Op { a: Box::new(self), b: Box::new(other), kind, bounds, depth }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncertain_curves_and_underflow_sized_holes_are_not_proven_covered() {
        let target = Rect::new(0., 0., 1e-200, 1e-200);
        assert!(!Region::Empty.contains(target), "a positive footprint is not empty when area underflows");
        let mut path = vectorcraft_geom::shapes::rectangle(Rect::new(0., 0., 100., 100.));
        path.subpaths[0].anchors[0].h_out.y += 1e-12;
        assert!(!Region::path(&path).contains(Rect::new(0., 0., 100., 100.)), "uncertain curved coverage is not a rectangle proof");
    }
}
