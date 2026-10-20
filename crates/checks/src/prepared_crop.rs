//! Exact rational retained crop boundaries; no pixel threshold or crop widening.
use serde::{Deserialize, Serialize};
use vectorcraft_geom::Rect;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    /// Reduced nonnegative numerator/positive denominator, in top-down pixel edges.
    pub edges: [[u64; 2]; 4],
    /// Absolute per-axis source-coordinate uncertainty, distinct from manufacturing tolerance.
    #[serde(default, skip_serializing_if = "zero_error")]
    pub edge_error_pixels: f64,
}
fn zero_error(v: &f64) -> bool {
    *v == 0.0
}
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
impl Clip {
    pub(crate) fn from_rect(raw: &vectorcraft_pdf::PrintRectCrop, width: u32, height: u32) -> Result<Self, String> {
        let result = Self { edges: raw.edges, edge_error_pixels: raw.edge_error_pixels };
        result.validate(width, height)?;
        Ok(result)
    }
    pub(crate) fn from_raw(bounds: [i64; 4], m: [i64; 6], width: u32, height: u32) -> Result<Self, String> {
        if bounds.iter().chain(m.iter()).any(|n| !(-(1 << 20)..=1 << 20).contains(n))
            || m[1] != 0
            || m[2] != 0
            || m[0] == 0
            || m[3] == 0
            || bounds[0] >= bounds[2]
            || bounds[1] >= bounds[3]
            || width == 0
            || height == 0
            || u64::from(width) * u64::from(height) > 1_048_576
        {
            return Err("Retained crop is outside the exact axis-aligned source domain".into());
        }
        let edge = |n: i64, d: i64, extent: u32| {
            let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
            let d = d as u64;
            let n = n.clamp(0, i64::from(extent) * d as i64) as u64;
            let g = gcd(n, d);
            [n / g, d / g]
        };
        let x = |v: i64| edge((v - m[4]) * i64::from(width), m[0], width);
        let y = |v: i64| edge((m[3] - v + m[5]) * i64::from(height), m[3], height);
        let (a, b, c, d) = (x(bounds[0]), y(bounds[1]), x(bounds[2]), y(bounds[3]));
        let (a, c) = if a[0] * c[1] <= c[0] * a[1] { (a, c) } else { (c, a) };
        let (b, d) = if b[0] * d[1] <= d[0] * b[1] { (b, d) } else { (d, b) };
        let result = Self { edges: [a, b, c, d], edge_error_pixels: 0.0 };
        result.validate(width, height)?;
        Ok(result)
    }
    pub(crate) fn validate(&self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 1_048_576 {
            return Err("Retained crop dimensions exceed budget".into());
        }
        if !self.edge_error_pixels.is_finite() || !(0.0..=0.000002).contains(&self.edge_error_pixels) {
            return Err("Retained crop source-coordinate uncertainty exceeds the bounded domain".into());
        }
        for ([n, d], extent) in self.edges.iter().zip([width, height, width, height]) {
            if *d == 0 || *d > (1u64 << 53) - 1 || *n > (1u64 << 53) - 1 || u128::from(*n) > u128::from(extent) * u128::from(*d) || gcd(*n, *d) != 1 {
                return Err("Retained crop fraction is noncanonical or outside the pixel grid".into());
            }
        }
        let [a, b, c, d] = self.edges;
        if u128::from(a[0]) * u128::from(c[1]) >= u128::from(c[0]) * u128::from(a[1])
            || u128::from(b[0]) * u128::from(d[1]) >= u128::from(d[0]) * u128::from(b[1])
        {
            return Err("Empty retained crop requires separate omission accounting".into());
        }
        Ok(())
    }
    pub(crate) fn intersects(&self, x: u32, y: u32) -> bool {
        let [a, b, c, d] = self.edges;
        u128::from(a[0]) < (u128::from(x) + 1) * u128::from(a[1])
            && u128::from(b[0]) < (u128::from(y) + 1) * u128::from(b[1])
            && u128::from(c[0]) > u128::from(x) * u128::from(c[1])
            && u128::from(d[0]) > u128::from(y) * u128::from(d[1])
    }
    pub(crate) fn rect(&self) -> Rect {
        let [a, b, c, d] = self.edges.map(|[n, d]| n as f64 / d as f64);
        Rect::new(a, b, c, d)
    }
}

pub(crate) fn compare(
    expected: &crate::prepared_mask::Mask,
    actual: &crate::prepared_mask::Mask,
    tolerance: Option<f64>,
    placement_error: f64,
) -> Result<crate::geometry::Measurement, String> {
    let t = tolerance.unwrap_or(0.0);
    let mut m = crate::geometry::compare(&expected.path()?, &actual.path()?, t, (t / 16.0).clamp(0.000001, 0.001))?;
    let error = expected.geometry_error_mm() + actual.geometry_error_mm() + placement_error;
    m.lower_mm = (m.lower_mm - error).max(0.0);
    m.upper_mm += error;
    m.uncertainty_mm = m.upper_mm - m.lower_mm;
    // Topology produced by a numerical mask union is not independently certified.
    // It cannot become Pass merely because the sampled boundary is nearby.
    m.status = if tolerance.is_some_and(|t| m.lower_mm > t) {
        crate::Status::Fail
    } else if tolerance.is_some_and(|t| m.upper_mm <= t) && m.expected_components == m.observed_components && m.expected_holes == m.observed_holes {
        crate::Status::Pass
    } else {
        crate::Status::Incomplete
    };
    m.reason = "Retained source crop boundary compared without alignment; mask construction and placement uncertainty included".into();
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_rational_edges_keep_partial_cells_and_exclude_only_hidden_cells() {
        let c = Clip::from_raw([19, 18, 40, 54], [36, 0, 0, 36, 18, 18], 3, 3).unwrap();
        assert_eq!(c.edges, [[1, 12], [0, 1], [11, 6], [3, 1]]);
        assert!(c.intersects(0, 0) && c.intersects(1, 2));
        assert!(!c.intersects(2, 0));
        let reflected = Clip::from_raw([19, 18, 40, 54], [-36, 0, 0, -36, 54, 54], 3, 3).unwrap();
        assert_eq!(reflected.edges, [[7, 6], [0, 1], [35, 12], [3, 1]]);
        assert!(!reflected.intersects(0, 0) && reflected.intersects(1, 0));
    }
    #[test]
    fn malformed_unbounded_empty_and_noncanonical_crop_proofs_are_refused() {
        for bounds in [[0, 0, 0, 2], [0, 0, 1, 1], [60, 60, 70, 70], [-1_048_577, 0, 40, 54]] {
            assert!(Clip::from_raw(bounds, [36, 0, 0, 36, 18, 18], 3, 3).is_err());
        }
        for edges in [
            [[0, 0], [0, 1], [3, 1], [3, 1]],
            [[0, 2], [0, 1], [3, 1], [3, 1]],
            [[0, 1], [0, 1], [4, 1], [3, 1]],
            [[0, 1], [0, 1], [1, 1u64 << 53], [3, 1]],
        ] {
            assert!(Clip { edges, edge_error_pixels: 0.0 }.validate(3, 3).is_err());
        }
    }
}
