//! Conservative control-hull clusters for independent distance-band construction.
//! Expanded hulls that touch or have unresolved numerical separation stay together.
use crate::feature_offset::WorkBudget;
use vectorcraft_geom::{PathData, Rect};

fn root(parents: &mut [usize], mut index: usize, budget: &mut WorkBudget) -> Result<usize, String> {
    let start = index;
    loop {
        budget.charge(1)?;
        let next = *parents.get(index).ok_or("Invalid feature cluster index")?;
        if next == index {
            break;
        }
        index = next;
    }
    // Compress every traversed node, not just the starting node. Reversed source
    // order can otherwise build a long chain and consume the report budget.
    let mut cursor = start;
    while cursor != index {
        budget.charge(1)?;
        let next = *parents.get(cursor).ok_or("Invalid feature cluster index")?;
        *parents.get_mut(cursor).ok_or("Invalid feature cluster index")? = index;
        cursor = next;
    }
    Ok(index)
}

pub(crate) fn split(path: &PathData, radius: f64, precision: f64, budget: &mut WorkBudget) -> Result<Vec<PathData>, String> {
    if !radius.is_finite() || radius < 0.0 || !precision.is_finite() || precision <= 0.0 || path.anchor_count() > 20_000 {
        return Err("Invalid or over-budget feature cluster input".into());
    }
    budget.charge(path.anchor_count().saturating_mul(3))?;
    let mut bounds = vec![];
    let mut scale = 1.0f64;
    for subpath in &path.subpaths {
        if !subpath.closed || subpath.anchors.len() < 2 {
            return Err("Unresolved feature cluster contour".into());
        }
        let mut hull: Option<Rect> = None;
        for anchor in &subpath.anchors {
            for point in [anchor.p, anchor.h_in, anchor.h_out] {
                if !point.x.is_finite() || !point.y.is_finite() || point.x.abs() > 1e6 || point.y.abs() > 1e6 {
                    return Err("Nonfinite feature cluster coordinate".into());
                }
                scale = scale.max(point.x.abs()).max(point.y.abs());
                hull = Some(hull.map_or(Rect::from_points(point, point), |r| r.union_pt(point)));
            }
        }
        bounds.push(hull.ok_or("Empty feature cluster contour")?);
    }
    // Control hulls contain the original curves; keep a margin for the bounded
    // curve approximation, sweep resolution and floating-point box arithmetic.
    let sweep = ((scale + radius + 1.0) * f64::EPSILON * 64.0).max(0.000001);
    let guard = precision + 2.0 * sweep + 1024.0 * f64::EPSILON * (scale + radius + 1.0);
    let expansion = radius + guard;
    for hull in &mut bounds {
        *hull = hull.inflate(expansion, expansion);
        if ![hull.x0, hull.y0, hull.x1, hull.y1].iter().all(|v| v.is_finite()) {
            return Err("Feature cluster extent overflow".into());
        }
    }
    let mut order: Vec<_> = bounds.iter().enumerate().map(|(i, b)| (i, *b)).collect();
    order.sort_by(|a, b| a.1.x0.total_cmp(&b.1.x0).then(a.0.cmp(&b.0)));
    let mut parents: Vec<_> = (0..bounds.len()).collect();
    let mut active: Vec<(usize, Rect)> = vec![];
    let mut comparisons = 0usize;
    for (index, hull) in order {
        budget.charge(active.len())?;
        active.retain(|(_, other)| other.x1 >= hull.x0);
        for (previous, other) in &active {
            comparisons += 1;
            if comparisons > 100_000 {
                return Err("Feature cluster pair-search budget exceeded".into());
            }
            budget.charge(1)?;
            if other.y1 < hull.y0 || hull.y1 < other.y0 {
                continue;
            }
            let a = root(&mut parents, index, budget)?;
            let b = root(&mut parents, *previous, budget)?;
            if a != b {
                *parents.get_mut(a.max(b)).ok_or("Invalid feature cluster root")? = a.min(b);
            }
        }
        active.push((index, hull));
    }
    let mut groups = std::collections::BTreeMap::<usize, PathData>::new();
    for (index, subpath) in path.subpaths.iter().enumerate() {
        let id = root(&mut parents, index, budget)?;
        groups.entry(id).or_default().subpaths.push(subpath.clone());
    }
    Ok(groups.into_values().collect())
}

/// Validate the actual constructed outputs before concatenating their contours.
/// A failure of the planar operation must not be hidden by input-only reasoning.
pub(crate) fn ensure_disjoint(paths: &[PathData], budget: &mut WorkBudget) -> Result<(), String> {
    let mut boxes = vec![];
    for path in paths {
        budget.charge(path.anchor_count().saturating_mul(3))?;
        let mut hull: Option<Rect> = None;
        let mut scale = 1.0f64;
        for subpath in &path.subpaths {
            if !subpath.closed || subpath.anchors.len() < 3 {
                return Err("Unresolved constructed cluster contour".into());
            }
            for anchor in &subpath.anchors {
                for point in [anchor.p, anchor.h_in, anchor.h_out] {
                    if !point.x.is_finite() || !point.y.is_finite() || point.x.abs() > 1e6 || point.y.abs() > 1e6 {
                        return Err("Invalid constructed cluster coordinate".into());
                    }
                    scale = scale.max(point.x.abs()).max(point.y.abs());
                    hull = Some(hull.map_or(Rect::from_points(point, point), |r| r.union_pt(point)));
                }
            }
        }
        if let Some(hull) = hull {
            let guard = 1024.0 * f64::EPSILON * (scale + 1.0);
            boxes.push(hull.inflate(guard, guard));
        }
    }
    boxes.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let mut comparisons = 0usize;
    for (i, a) in boxes.iter().enumerate() {
        for b in boxes.iter().skip(i + 1) {
            if b.x0 > a.x1 {
                break;
            }
            comparisons += 1;
            budget.charge(1)?;
            if comparisons > 100_000 {
                return Err("Constructed cluster pair-search budget exceeded".into());
            }
            if a.y0 <= b.y1 && b.y0 <= a.y1 {
                return Err("Constructed cluster bounds touch or overlap; concatenation refused".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_geom::Shape;
    fn rect(x: f64, y: f64, w: f64, h: f64) -> PathData {
        PathData::from_bezpath(&Rect::new(x, y, x + w, y + h).to_path(0.000001))
    }
    fn joined(paths: impl IntoIterator<Item = PathData>) -> PathData {
        PathData::new(paths.into_iter().flat_map(|p| p.subpaths).collect())
    }
    #[test]
    fn nested_hole_and_island_stay_with_outer_even_when_listed_first() {
        let mut hole = rect(1.0, 1.0, 2.0, 2.0);
        hole.reverse();
        let input = joined([hole, rect(0.0, 0.0, 4.0, 4.0), rect(1.5, 1.5, 0.2, 0.2), rect(10.0, 0.0, 1.0, 1.0)]);
        let result = split(&input, 0.1, 0.0001, &mut WorkBudget::new()).unwrap();
        assert_eq!(result.iter().map(|p| p.subpaths.len()).collect::<Vec<_>>(), vec![3, 1]);
        assert_eq!(result[0].subpaths[0], input.subpaths[0]);
        assert_eq!(result[1].subpaths[0], input.subpaths[3]);
    }
    #[test]
    fn contact_and_uncertain_separation_merge_at_negative_and_large_coordinates() {
        for origin in [-100.0, 0.0, 999_900.0] {
            for (gap, count) in [(0.2, 1), (0.2000001, 1), (0.201, 2)] {
                let p = joined([rect(origin, 0.0, 1.0, 1.0), rect(origin + 1.0 + gap, 0.0, 1.0, 1.0)]);
                assert_eq!(split(&p, 0.1, 0.0001, &mut WorkBudget::new()).unwrap().len(), count, "{origin} {gap}");
            }
        }
        let mut a = rect(0.0, 0.0, 1.0, 1.0);
        a.subpaths[0].anchors[0].h_out.x = 10.0;
        assert_eq!(split(&joined([a, rect(9.0, 0.0, 1.0, 1.0)]), 0.1, 0.0001, &mut WorkBudget::new()).unwrap().len(), 1);
    }
    #[test]
    fn malformed_geometry_and_quadratic_candidate_search_refuse() {
        for mutation in [0, 1, 2, 3] {
            let mut p = rect(0.0, 0.0, 1.0, 1.0);
            match mutation {
                0 => p.subpaths[0].closed = false,
                1 => p.subpaths[0].anchors.clear(),
                2 => p.subpaths[0].anchors[0].p.x = f64::NAN,
                _ => p.subpaths[0].anchors[0].h_out.x = 1e6 + 1.0,
            }
            assert!(split(&p, 0.1, 0.0001, &mut WorkBudget::new()).is_err());
        }
        let p = joined((0..501).map(|i| rect(0.0, f64::from(i) * 3.0, 1.0, 1.0)));
        assert!(split(&p, 0.1, 0.0001, &mut WorkBudget::new()).unwrap_err().contains("pair-search"));
    }
    #[test]
    fn constructed_outputs_must_be_proven_disjoint_before_concatenation() {
        let a = rect(0.0, 0.0, 1.0, 1.0);
        for x in [0.5, 1.0, 1.0 + f64::EPSILON] {
            assert!(ensure_disjoint(&[a.clone(), rect(x, 0.0, 1.0, 1.0)], &mut WorkBudget::new()).is_err());
        }
        ensure_disjoint(&[a, PathData::default(), rect(1.001, 0.0, 1.0, 1.0)], &mut WorkBudget::new()).unwrap();
    }
    #[test]
    fn reverse_ordered_connected_contours_do_not_exhaust_the_report_budget() {
        let p = joined((0..2000).rev().map(|i| rect(f64::from(i) * 0.05, 0.0, 0.1, 0.1)));
        let mut budget = WorkBudget::new();
        let result = split(&p, 0.1, 0.0001, &mut budget).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].subpaths, p.subpaths);
        // Reserve most of the shared budget for subsequent plates and stages.
        assert!(budget.charge(1_500_000).is_ok());
    }
    #[test]
    fn concavity_and_shuffled_contours_preserve_complete_groups() {
        let c = PathData::from_bezpath(&vectorcraft_geom::BezPath::from_vec(vec![
            vectorcraft_geom::PathEl::MoveTo((0.0, 0.0).into()),
            vectorcraft_geom::PathEl::LineTo((4.0, 0.0).into()),
            vectorcraft_geom::PathEl::LineTo((4.0, 1.0).into()),
            vectorcraft_geom::PathEl::LineTo((1.0, 1.0).into()),
            vectorcraft_geom::PathEl::LineTo((1.0, 3.0).into()),
            vectorcraft_geom::PathEl::LineTo((4.0, 3.0).into()),
            vectorcraft_geom::PathEl::LineTo((4.0, 4.0).into()),
            vectorcraft_geom::PathEl::LineTo((0.0, 4.0).into()),
            vectorcraft_geom::PathEl::ClosePath,
        ]));
        let island = rect(2.0, 1.5, 0.2, 0.2);
        let far = rect(10.0, 0.0, 1.0, 1.0);
        for input in [joined([c.clone(), island.clone(), far.clone()]), joined([far.clone(), island.clone(), c.clone()])] {
            let groups = split(&input, 0.1, 0.0001, &mut WorkBudget::new()).unwrap();
            assert_eq!(groups.len(), 2);
            let near = groups.iter().find(|g| g.subpaths.len() == 2).unwrap();
            assert!(near.subpaths.contains(&c.subpaths[0]));
            assert!(near.subpaths.contains(&island.subpaths[0]));
            assert!(groups.iter().any(|g| g == &far));
        }
    }
    #[test]
    fn adjacent_representable_positions_straddle_the_guarded_contact_threshold() {
        // Find the exact floating-point transition, then check its two adjacent
        // representable coordinates. The coarse bracket is independently known.
        let groups = |x| split(&joined([rect(0.0, 0.0, 1.0, 1.0), rect(x, 0.0, 1.0, 1.0)]), 0.1, 0.0001, &mut WorkBudget::new()).unwrap().len();
        let mut low = 1.2f64.to_bits();
        let mut high = 1.201f64.to_bits();
        assert_eq!(groups(f64::from_bits(low)), 1);
        assert_eq!(groups(f64::from_bits(high)), 2);
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            if groups(f64::from_bits(mid)) == 1 {
                low = mid;
            } else {
                high = mid;
            }
        }
        assert_eq!(groups(f64::from_bits(low)), 1);
        assert_eq!(groups(f64::from_bits(high)), 2);
    }
}
