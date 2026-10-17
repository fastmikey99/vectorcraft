//! Original synthetic regressions for insets of compound outlines; no customer geometry.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use kurbo::BezPath;
use vectorcraft_geom::{FillRule, PathData};
use vectorcraft_pathops::{Join, PathOpsError, area, offset_path, try_offset_path};

#[test]
fn an_inset_keeps_an_outline_whose_widest_span_crosses_its_hole() {
    // A square with a thin rectangular counter. The contour-only probe lands in the
    // counter, closer than the inset distance to the source, and used to drop the square.
    let path = PathData::from_bezpath(&BezPath::from_svg("M0 0 H100 V100 H0 Z M10 49.9 V50.1 H90 V49.9 Z").unwrap());
    let before = area(&path, FillRule::NonZero);
    for delta in [-0.28, -0.29, -0.30, -1.0] {
        let inset = try_offset_path(&path, delta, Join::Round, 4.0).unwrap();
        let after = area(&inset, FillRule::NonZero);
        assert!(after > before * 0.90 && after < before, "{delta}: {before} → {after}");
        assert_eq!(inset.subpaths.len(), 2, "the outline and its counter");
    }
}

#[test]
fn fallible_offset_distinguishes_invalid_input_from_a_consumed_inset() {
    let path = PathData::from_bezpath(&BezPath::from_svg("M0 0 H10 V10 H0 Z").unwrap());
    assert!(matches!(try_offset_path(&path, f64::NAN, Join::Round, 4.0), Err(PathOpsError::NonFinite)));
    assert!(offset_path(&path, f64::NAN, Join::Round, 4.0).is_empty());
    assert!(try_offset_path(&path, -20.0, Join::Round, 4.0).unwrap().is_empty());
    assert!(try_offset_path(&PathData::default(), 1.0, Join::Round, 4.0).unwrap().is_empty());
}
