//! A bounded polygonal distance-band construction independent of the editor's
//! stroker/face-sampling offset. Capsules are consistently oriented and united
//! before the band is added to or removed from the original filled region.
use crate::geometry::Boundary;
use vectorcraft_geom::{BezPath, FillRule, PathData, Point};
use vectorcraft_pathops::BoolOp;

/// Aggregate construction work across a report, in retained/generated vertices.
/// The external worker deadline remains necessary: the planar sweep itself has
/// no cooperative iteration budget and these bounds are not a termination proof.
pub(crate) struct WorkBudget {
    remaining: usize,
}
impl WorkBudget {
    pub fn new() -> Self {
        Self { remaining: 2_000_000 }
    }
    pub fn charge(&mut self, amount: usize) -> Result<(), String> {
        if amount > self.remaining {
            self.remaining = 0;
            return Err("Aggregate physical-feature work budget exceeded".into());
        }
        self.remaining -= amount;
        Ok(())
    }
}

pub(crate) struct Offset {
    pub path: PathData,
    pub band_approximation_mm: f64,
}
#[cfg(test)]
pub(crate) fn offset(path: &PathData, delta: f64, precision: f64) -> Result<Offset, String> {
    offset_with_budget(path, delta, precision, &mut WorkBudget::new())
}
pub(crate) fn offset_with_budget(path: &PathData, delta: f64, precision: f64, budget: &mut WorkBudget) -> Result<Offset, String> {
    budget.charge(path.anchor_count())?;
    if !delta.is_finite() || !precision.is_finite() || !(0.000001..=0.001).contains(&precision) || path.anchor_count() > 20_000 {
        return Err("Invalid or over-budget distance-band input".into());
    }
    if path.is_empty() || delta == 0.0 {
        return Ok(Offset { path: path.clone(), band_approximation_mm: 0.0 });
    }
    let boundary = Boundary::new(path, FillRule::NonZero, precision)?;
    let radius = delta.abs();
    let mut sides = 8usize;
    while radius * (1.0 - (std::f64::consts::PI / sides as f64).cos()) > precision {
        sides = sides.saturating_mul(2);
        if sides > 4096 {
            return Err("Distance-band circle approximation budget exceeded".into());
        }
    }
    let count = boundary.linear_segments().count();
    if count.saturating_mul(sides + 2) > 200_000 {
        return Err("Distance-band aggregate polygon budget exceeded".into());
    }
    budget.charge(count.saturating_mul(sides + 2))?;
    let mut band = PathData::default();
    for (a, b) in boundary.linear_segments() {
        let theta = (b.y - a.y).atan2(b.x - a.x);
        let mut capsule = BezPath::new();
        for (center, start) in [(b, theta - std::f64::consts::FRAC_PI_2), (a, theta + std::f64::consts::FRAC_PI_2)] {
            for step in 0..=sides / 2 {
                let angle = start + std::f64::consts::TAU * step as f64 / sides as f64;
                let p = Point::new(center.x + radius * angle.cos(), center.y + radius * angle.sin());
                if capsule.elements().is_empty() {
                    capsule.move_to(p);
                } else {
                    capsule.line_to(p);
                }
            }
        }
        capsule.close_path();
        band.subpaths.extend(PathData::from_bezpath(&capsule).subpaths);
    }
    let sweep = vectorcraft_pathops::measurement_boolean_epsilon(path, &band).map_err(|e| e.to_string())?;
    let mut out = vectorcraft_pathops::try_boolean_for_measurement(
        path,
        FillRule::NonZero,
        &band,
        FillRule::NonZero,
        if delta > 0.0 { BoolOp::Union } else { BoolOp::Difference },
    )
    .map_err(|e| e.to_string())?;
    // Planar intersection can emit consecutive polygon vertices differing only
    // by floating-point roundoff. Remove those construction-only zero-length
    // edges, retain every contour, and explicitly retain the displacement bound.
    // Never apply this cleanup to input artwork or curved edges.
    let scale = out.subpaths.iter().flat_map(|s| &s.anchors).map(|a| a.p.x.abs().max(a.p.y.abs())).fold(1.0, f64::max);
    let roundoff = 256.0 * f64::EPSILON * scale;
    for subpath in &mut out.subpaths {
        if subpath.anchors.iter().any(|a| a.h_in != a.p || a.h_out != a.p) {
            continue;
        }
        subpath.anchors.dedup_by(|a, b| a.p.distance(b.p) <= roundoff);
        if subpath.anchors.len() > 1 && subpath.anchors.first().zip(subpath.anchors.last()).is_some_and(|(a, b)| a.p.distance(b.p) <= roundoff) {
            subpath.anchors.pop();
        }
        if subpath.anchors.len() < 3 {
            return Err("Roundoff-degenerate constructed contour remains unresolved".into());
        }
    }
    if out.anchor_count() > 100_000 {
        return Err("Distance-band result budget exceeded".into());
    }
    budget.charge(out.anchor_count())?;
    Ok(Offset {
        path: out,
        band_approximation_mm: sweep + roundoff + boundary.linear_error_mm() + radius * (1.0 - (std::f64::consts::PI / sides as f64).cos()),
    })
}
