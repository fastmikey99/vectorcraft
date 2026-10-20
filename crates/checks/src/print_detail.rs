//! Exact minimum widths and clear gaps for final plates consisting of disjoint axis-aligned rectangles.
//! Curves, holes, process channels and raster halftones retain specific measurement limitations.
use crate::{Request, Status, pdf::Census};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use vectorcraft_geom::{PathData, Rect, Shape};
fn rectangles(path: &PathData) -> Option<Vec<Rect>> {
    if path.subpaths.is_empty() || path.subpaths.len() > 200 {
        return None;
    }
    let mut rects = vec![];
    let mut direction = None;
    for subpath in &path.subpaths {
        if !subpath.closed || subpath.anchors.len() != 4 || subpath.anchors.iter().any(|a| a.h_in != a.p || a.h_out != a.p) {
            return None;
        }
        let shape = PathData::single(subpath.clone()).to_bezpath();
        let b = shape.bounding_box();
        let sign = shape.area().is_sign_positive();
        if direction.is_some_and(|v| v != sign) {
            return None;
        }
        direction = Some(sign);
        let corners = BTreeSet::from([(0, 0), (0, 1), (1, 0), (1, 1)]);
        let found: Option<BTreeSet<_>> = subpath
            .anchors
            .iter()
            .map(|a| {
                Some((
                    if a.p.x == b.x0 {
                        0
                    } else if a.p.x == b.x1 {
                        1
                    } else {
                        return None;
                    },
                    if a.p.y == b.y0 {
                        0
                    } else if a.p.y == b.y1 {
                        1
                    } else {
                        return None;
                    },
                ))
            })
            .collect();
        if found.as_ref() != Some(&corners) || b.width() <= 0.0 || b.height() <= 0.0 {
            return None;
        }
        // Distinct corners alone could be a self-crossing bowtie.
        if (shape.area().abs() - b.area()).abs() > 1e-10 {
            return None;
        }
        rects.push(b);
    }
    for (i, a) in rects.iter().enumerate() {
        for b in rects.iter().skip(i + 1) {
            if a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1 {
                return None;
            }
        }
    }
    Some(rects)
}
fn widths(rects: &[Rect]) -> (f64, Option<f64>) {
    let width = rects.iter().map(|b| b.width().min(b.height())).fold(f64::INFINITY, f64::min);
    let mut gap = f64::INFINITY;
    for (i, a) in rects.iter().enumerate() {
        for b in rects.iter().skip(i + 1) {
            let dx = (a.x0 - b.x1).max(b.x0 - a.x1).max(0.0);
            let dy = (a.y0 - b.y1).max(b.y0 - a.y1).max(0.0);
            gap = gap.min(dx.hypot(dy));
        }
    }
    (width, gap.is_finite().then_some(gap))
}
pub(crate) fn measure(r: &Request, c: &Census) -> (Status, String, Value) {
    let limit = |key: &str| r.acceptance.get(key).and_then(|f| f.value.as_f64()).filter(|v| v.is_finite() && *v > 0.0);
    let detail = limit("minimum_detail_mm");
    let gap_limit = limit("minimum_gap_mm");
    let inks: BTreeSet<_> = c.paints.iter().filter_map(|p| p.ink.as_deref()).collect();
    let mut rows = vec![];
    let mut limits = vec![];
    let mut failed = false;
    if c.paints.iter().any(|p| p.process) {
        limits.push("Process-colour detail and halftone gaps are not measured by the solid spot-plate model".to_string());
    }
    let mut budget = crate::feature_offset::WorkBudget::new();
    for ink in inks.iter().take(64) {
        for page in 0..c.pages.min(16) {
            let coverage = match crate::coverage::practical_solid(c, page, ink, &mut budget) {
                Ok(v) => v,
                Err(_) => {
                    limits.push(format!(
                        "{ink}, page {}: images, partial tones, masks or unresolved paint order prevent measuring final solid-ink detail",
                        page + 1
                    ));
                    continue;
                }
            };
            if coverage.path.is_empty() {
                continue;
            }
            let Some(rects) = rectangles(&coverage.path) else {
                limits.push(format!("{ink}, page {}: curved/nonrectangular contours or holes need a general minimum-feature measurement", page + 1));
                continue;
            };
            let (width, gap) = widths(&rects);
            let allowance = coverage.construction_allowance_mm * 2.0;
            let thin = detail.is_some_and(|v| width + allowance < v);
            let narrow = gap.zip(gap_limit).is_some_and(|(g, v)| g + allowance < v);
            failed |= thin || narrow;
            rows.push(json!({"ink":ink,"page":page,"minimum_detail_mm":[(width-allowance).max(0.0),width+allowance],"minimum_clear_gap_mm":gap.map(|g|[(g-allowance).max(0.0),g+allowance]),"detail_below_limit":thin,"gap_below_limit":narrow}));
        }
    }
    if inks.len() > 64 || c.pages > 16 {
        limits.push("Plate/page measurement budget exceeded".into());
    }
    let pass = !rows.is_empty()
        && limits.is_empty()
        && detail.zip(gap_limit).is_some_and(|(d, g)| {
            rows.iter().all(|row| {
                row["minimum_detail_mm"][0].as_f64().is_some_and(|w| w >= d)
                    && (row["minimum_clear_gap_mm"].is_null() || row["minimum_clear_gap_mm"][0].as_f64().is_some_and(|w| w >= g))
            })
        });
    let reason = if failed {
        "A measured line/detail or clear gap is below the supplied manufacturing minimum"
    } else if pass {
        "All measured solid rectangular plate details and clear gaps meet the supplied minima"
    } else if rows.is_empty() {
        "No complete solid rectangular plate could be measured; specific limits are listed"
    } else if detail.is_none() || gap_limit.is_none() {
        "Measured solid-plate detail and gaps retained; manufacturing minima were not supplied"
    } else {
        "Measured detail retained; some geometry or numerical bounds remain unresolved"
    };
    (
        if failed {
            Status::Fail
        } else if pass {
            Status::Pass
        } else {
            Status::Incomplete
        },
        reason.into(),
        json!({"minimum_detail_requirement_mm":detail,"minimum_gap_requirement_mm":gap_limit,"measurements":rows,"limitations":limits}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rectangular_minima_and_gaps_are_geometric_not_bounding_box_guesses() {
        let a = PathData::from_bezpath(&Rect::new(0.0, 0.0, 0.1, 2.0).to_path(0.0001));
        let mut pair = a.clone();
        pair.subpaths.extend(PathData::from_bezpath(&Rect::new(0.15, 0.0, 1.0, 2.0).to_path(0.0001)).subpaths);
        let (width, gap) = widths(&rectangles(&pair).unwrap());
        assert!((width - 0.1).abs() < 1e-10);
        assert!((gap.unwrap() - 0.05).abs() < 1e-10);
        let mut bez = vectorcraft_geom::BezPath::new();
        bez.move_to((0.0, 0.0));
        bez.quad_to((1.0, 2.0), (2.0, 0.0));
        bez.close_path();
        let curve = PathData::from_bezpath(&bez);
        assert!(rectangles(&curve).is_none());
    }
}
