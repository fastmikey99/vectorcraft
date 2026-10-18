//! Replay explicit preparation decisions against a fresh original. Nothing is aligned
//! to the finished separation, and every edited region remains in the original census.
use crate::{
    digest,
    preparation::{Fact, Provenance},
    prepare::{Draft, Preview},
};
use serde::Deserialize;
use std::collections::BTreeSet;
use vectorcraft_geom::{Affine, FillRule, PathData, Rect, Shape};
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Edit {
    RasterMask { source: String, source_sha256: String, policy: String },
    Affine { regions: Vec<String>, matrix: [f64; 6], target_page: Option<usize> },
    Crop { regions: Vec<String>, bounds_mm: [f64; 4] },
}
/// Replay only the source pixel grid. Crops need a separate sample/clip proof;
/// never recover a transform by fitting the finished output's bounding box.
pub(crate) fn raster_placement(
    mask: &crate::prepared_mask::Mask,
    id: &str,
    edits: &[Fact],
) -> Result<(crate::prepared_mask::Mask, PathData, f64), String> {
    if edits.len() > 1000 {
        return Err("Raster placement edit budget exceeded".into());
    }
    let mut placed = mask.clone();
    let mut path = mask.path()?;
    let mut error = 1.0e-9;
    let mut approved = false;
    for fact in edits {
        if !fact.evidenced() || !matches!(fact.provenance, Provenance::Operator { .. }) {
            return Err("Raster placement requires attributed preparation decisions".into());
        }
        match serde_json::from_value::<Edit>(fact.value.clone()).map_err(|e| e.to_string())? {
            Edit::RasterMask { source, source_sha256, policy } if source == id => {
                if approved || source_sha256 != mask.source_sha256 || policy != "exact_binary_alpha" {
                    return Err("Raster source boundary decision is invalid".into());
                }
                approved = true;
            }
            Edit::Affine { regions, matrix, target_page } if regions.iter().any(|r| r == id) => {
                if !approved {
                    return Err("Raster transform precedes its boundary decision".into());
                }
                let (xf, norm) = checked_affine(matrix, target_page)?;
                // Bound coefficient composition before cancellation, then evaluate
                // its effect across the full original pixel grid.
                let magnitude =
                    matrix.iter().map(|n| n.abs()).fold(1.0, f64::max) * placed.pixel_to_page_mm.iter().map(|n| n.abs()).fold(1.0, f64::max);
                error = norm * error + 128.0 * f64::EPSILON * magnitude * (f64::from(mask.width) + f64::from(mask.height) + 1.0);
                if !error.is_finite() {
                    return Err("Raster affine uncertainty exceeds finite bounds".into());
                }
                path = path.transformed(xf);
                placed.pixel_to_page_mm = (xf * Affine::new(placed.pixel_to_page_mm)).as_coeffs();
                if let Some(page) = target_page {
                    placed.page = page;
                }
                placed.validate()?;
            }
            Edit::Crop { regions, .. } if regions.iter().any(|r| r == id) => {
                return Err("Cropped raster source continuity needs explicit sample/clip reconciliation".into());
            }
            _ => {}
        }
    }
    if !approved {
        return Err("Raster source boundary decision is missing".into());
    }
    placed.validate()?;
    Ok((placed, path, error))
}
fn checked_affine(matrix: [f64; 6], target_page: Option<usize>) -> Result<(Affine, f64), String> {
    if matrix.iter().any(|n| !n.is_finite() || n.abs() > 1_000_000.0) || target_page.is_some_and(|p| p >= 1000) {
        return Err("Preparation transform exceeds finite coordinate/page bounds".into());
    }
    let xf = Affine::new(matrix);
    if !xf.determinant().is_finite() || xf.determinant() == 0.0 {
        return Err("Preparation transform is singular".into());
    }
    let [a, b, c, d, _, _] = matrix;
    let norm = ((a.abs() + b.abs()).max(c.abs() + d.abs()) * (a.abs() + c.abs()).max(b.abs() + d.abs())).sqrt();
    Ok((xf, norm))
}
pub fn replay(mut draft: Draft, edits: &[Fact]) -> Result<Draft, String> {
    if edits.len() > 1000 || !draft.baseline.intentional_edits.is_empty() {
        return Err("Preparation edit replay budget or origin invalid".into());
    }
    let mut geometry_edited = false;
    for fact in edits {
        if !fact.evidenced() || !matches!(fact.provenance, Provenance::Operator { .. }) {
            return Err("Intentional preparation changes require explicit attributed operator decisions".into());
        }
        let edit: Edit = serde_json::from_value(fact.value.clone()).map_err(|e| format!("Unsupported preparation edit: {e}"))?;
        if let Edit::RasterMask { source, source_sha256, policy } = &edit {
            crate::raster_prepare::apply(&mut draft, source, source_sha256, policy)?;
            continue;
        }
        geometry_edited = true;
        let regions = match &edit {
            Edit::RasterMask { .. } => return Err("Undispatched raster mask decision".into()),
            Edit::Affine { regions, .. } | Edit::Crop { regions, .. } => regions,
        };
        if regions.is_empty() || regions.len() > 50_000 || regions.iter().collect::<BTreeSet<_>>().len() != regions.len() {
            return Err("Preparation edit needs unique captured region identities".into());
        }
        let (xf, crop, target_page, error_scale) = match &edit {
            Edit::RasterMask { .. } => return Err("Undispatched raster mask decision".into()),
            Edit::Affine { matrix, target_page, .. } => {
                let (xf, norm) = checked_affine(*matrix, *target_page)?;
                (Some(xf), None, *target_page, norm)
            }
            Edit::Crop { bounds_mm, .. } => {
                let [x0, y0, x1, y1] = *bounds_mm;
                if bounds_mm.iter().any(|n| !n.is_finite() || n.abs() > 1_000_000.0) || x1 <= x0 || y1 <= y0 {
                    return Err("Invalid preparation crop rectangle".into());
                }
                (None, Some(PathData::from_bezpath(&Rect::new(x0, y0, x1, y1).to_path(0.000001))), None, 1.0)
            }
        };
        for id in regions {
            let geometry = draft.geometry.regions.get_mut(id).ok_or("Preparation edit references an unknown source region")?;
            if let Some(xf) = xf {
                geometry.path_mm = geometry.path_mm.transformed(xf);
            }
            if let Some(clip) = &crop {
                geometry.path_mm = vectorcraft_pathops::try_boolean_for_measurement(
                    &geometry.path_mm,
                    geometry.fill_rule,
                    clip,
                    FillRule::NonZero,
                    vectorcraft_pathops::BoolOp::Intersect,
                )
                .map_err(|e| e.to_string())?;
                geometry.fill_rule = FillRule::NonZero;
            }
            if let Some(page) = target_page {
                geometry.page = page;
            }
            geometry.geometry_error_mm = geometry.geometry_error_mm * error_scale + if crop.is_some() { 0.00001 } else { 0.000000001 };
            let path = geometry.path_mm.to_bezpath();
            let b = path.bounding_box();
            let bounds = [b.x0, b.y0, b.x1, b.y1];
            if bounds.iter().any(|v| !v.is_finite() || v.abs() > 1_000_000.0) || !geometry.geometry_error_mm.is_finite() {
                return Err("Transformed preparation exceeds coordinate bounds".into());
            }
            let region = draft.baseline.regions.iter_mut().find(|r| &r.id == id).ok_or("Prepared region missing from census")?;
            region.geometry_sha256 = digest(geometry)?;
            draft.previews.insert(id.clone(), Preview { page: geometry.page, bounds_mm: bounds, path_svg: path.to_svg() });
        }
    }
    if geometry_edited {
        draft.baseline.selected_pages = draft.geometry.regions.values().map(|g| g.page).collect::<BTreeSet<_>>().into_iter().collect();
    }
    draft.baseline.intentional_edits = edits.to_vec();
    draft.baseline.approval = None;
    Ok(draft)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fact(value: serde_json::Value) -> Fact {
        Fact {
            value,
            provenance: Provenance::Operator {
                actor: "Synthetic fixture".into(),
                at: "2026-10-18".into(),
                decision_id: "explicit preparation".into(),
                evidence_sha256: "e".repeat(64),
            },
        }
    }
    #[test]
    fn explicit_crop_scale_mirror_and_placement_replay_without_output_alignment() {
        let bytes = crate::pdf::tests::pdf(true);
        let original = crate::prepare::capture(&bytes, "capture".into(), "job".into()).unwrap();
        let ids: Vec<_> = original.geometry.regions.keys().cloned().collect();
        let edits = vec![
            fact(json!({"kind":"crop","regions":ids,"bounds_mm":[0,0,20,30]})),
            fact(json!({"kind":"affine","regions":ids,"matrix":[-2,0,0,2,50,5],"target_page":1})),
        ];
        let edited = replay(original.clone(), &edits).unwrap();
        assert_eq!(edited.baseline.source_census, original.baseline.source_census);
        assert_eq!(edited.baseline.native_sha256, original.baseline.native_sha256);
        assert_eq!(edited.baseline.selected_pages, vec![1]);
        for (id, g) in &edited.geometry.regions {
            assert_eq!(g.paint, original.geometry.regions[id].paint);
            assert_eq!(g.page, 1);
            assert!(g.path_mm.to_bezpath().bounding_box().x0 >= 10.0);
        }
        let mut request = crate::attribution::tests::request();
        request.identity.original_sha256 = crate::sha256(&bytes);
        request.baseline = Some(edited.baseline);
        request.geometry = Some(edited.geometry);
        assert!(crate::prepare::verify_original(&request, &bytes).unwrap().is_empty());
        request.baseline.as_mut().unwrap().intentional_edits.clear();
        assert!(!crate::prepare::verify_original(&request, &bytes).unwrap().is_empty());
    }
    #[test]
    fn unknown_singular_and_unattributed_edits_are_rejected() {
        let original = crate::prepare::capture(&crate::pdf::tests::pdf(true), "capture".into(), "job".into()).unwrap();
        let ids: Vec<_> = original.geometry.regions.keys().cloned().collect();
        let mut unauthorised = fact(json!({"kind":"crop","regions":ids,"bounds_mm":[0,0,1,1]}));
        unauthorised.provenance = Provenance::Derived { artifact_sha256: "a".repeat(64), algorithm: "unapproved-crop".into(), version: "1".into() };
        assert!(replay(original.clone(), &[unauthorised]).is_err());
        for value in [
            json!({"kind":"auto_fit_output"}),
            json!({"kind":"affine","regions":ids,"matrix":[0,0,0,0,0,0],"target_page":null}),
            json!({"kind":"crop","regions":["unknown"],"bounds_mm":[0,0,1,1]}),
        ] {
            assert!(replay(original.clone(), &[fact(value)]).is_err());
        }
    }
}
