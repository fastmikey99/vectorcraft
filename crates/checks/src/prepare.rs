//! Capture an original independently of the finished separation. No automatic approval or
//! role inference from an ink's appearance. Geometry remains bound to the exact original.
use crate::{
    Evidence, digest, pdf,
    preparation::{Baseline, Fact, Provenance, Region},
    program::{Inputs, RegionGeometry},
    sha256,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use vectorcraft_geom::Shape;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub page: usize,
    pub bounds_mm: [f64; 4],
    pub path_svg: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub raster_sources: BTreeMap<String, crate::raster_prepare::Source>,
    pub baseline: Baseline,
    pub geometry: Inputs,
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
    pub previews: BTreeMap<String, Preview>,
}
pub fn capture(bytes: &[u8], id: String, job_revision: String) -> Result<Draft, String> {
    if bytes.len() > 256 * 1024 * 1024 || id.trim().is_empty() || job_revision.trim().is_empty() {
        return Err("Original exceeds budget or preparation/job identity is missing".into());
    }
    let imported = vectorcraft_pdf::import_with_report(
        bytes,
        &vectorcraft_pdf::ImportOptions { reject_depth_loss: true, text_as: vectorcraft_pdf::TextAs::Outlines, ..Default::default() },
    )
    .map_err(|e| e.to_string())?;
    let mut census = pdf::census(&imported.document);
    let ordered = crate::ordering::try_reconcile(bytes, &mut census).is_ok();
    let original = sha256(bytes);
    let (mut diagnostics, notices) = pdf::import_messages(imported.warnings);
    let import_issues = diagnostics.clone();
    let native_images = census.image_objects;
    diagnostics.extend(census.unresolved);
    if census.budget_exhausted {
        diagnostics.push("Original census resource budget exceeded".into());
    }
    let mut baseline = Baseline {
        id,
        revision: 1,
        original_sha256: original.clone(),
        native_sha256: digest(&imported.document)?,
        job_revision,
        selected_pages: (0..census.pages).collect(),
        source_census: census.unaccounted_objects,
        census_evidence: Evidence {
            artifact_sha256: original.clone(),
            producer: "checks/original-public-pdf-1".into(),
            detail: "Fresh original public content import; unsupported content remains in diagnostics".into(),
        },
        regions: vec![],
        intentional_edits: vec![],
        approval: None,
    };
    let mut geometry = Inputs::default();
    let mut previews = BTreeMap::new();
    for paint in census.paints {
        let key = format!("node-{}-paint-{}", paint.id, paint.order);
        baseline.source_census.push(key.clone());
        let (Some(page), Some(path_mm)) = (paint.page, paint.geometry_mm) else {
            diagnostics.push(format!("{key}: explicit prepared geometry/mask required"));
            continue;
        };
        let artifact = RegionGeometry {
            prepared_mask: None,
            page,
            path_mm,
            fill_rule: vectorcraft_geom::FillRule::NonZero,
            paint: paint.paint.clone(),
            geometry_error_mm: paint.geometry_error_mm,
            source_order: ordered.then_some(paint.order),
        };
        let path = artifact.path_mm.to_bezpath();
        let b = path.bounding_box();
        previews.insert(key.clone(), Preview { page, bounds_mm: [b.x0, b.y0, b.x1, b.y1], path_svg: path.to_svg() });
        let mut facts = BTreeMap::new();
        let provenance = Provenance::File { artifact_sha256: original.clone(), locator: key.clone() };
        let mut fact = |name: &str, value: Value| {
            facts.insert(name.into(), Fact { value, provenance: provenance.clone() });
        };
        if let Some(ink) = paint.ink {
            fact("ink", Value::String(ink));
        }
        if let Some(model) = paint.paint.get("color").and_then(|c| c.get("model")).and_then(Value::as_str) {
            fact("paint", Value::String(model.into()));
        }
        // The editable importer stores f32 channels. Their sum cannot prove an exact PDF
        // CMYK-total predicate: keep that assignment unresolved until exact source/operator evidence.
        baseline.regions.push(Region {
            id: key.clone(),
            source_objects: vec![key.clone()],
            geometry_sha256: digest(&artifact)?,
            facts,
            prepared_mask_sha256: None,
        });
        geometry.regions.insert(key, artifact);
    }
    if baseline.source_census.is_empty() {
        diagnostics.push("No fully accounted printable original content".into());
    }
    let mut draft = Draft { baseline, geometry, diagnostics, notices, previews, raster_sources: BTreeMap::new() };
    if native_images > 0
        && !census.budget_exhausted
        && let Err(reason) = crate::raster_prepare::capture(bytes, &mut draft, native_images, &import_issues)
    {
        draft.diagnostics.push(format!("Raster preparation remains unresolved: {reason}"));
    }
    Ok(draft)
}
/// Verify a stored preparation against a fresh read of its original. Intentional geometry edits
/// require a distinct adjudicated artifact; they cannot quietly make output its own baseline.
pub fn verify_original(request: &crate::Request, bytes: &[u8]) -> Result<Vec<String>, String> {
    let b = request.baseline.as_ref().ok_or("Preparation baseline missing")?;
    let mut issues = vec![];
    if sha256(bytes) != request.identity.original_sha256 || b.original_sha256 != request.identity.original_sha256 {
        return Ok(vec!["Original byte identity mismatch".into()]);
    }
    let fresh = match crate::prepare_edits::replay(capture(bytes, b.id.clone(), b.job_revision.clone())?, &b.intentional_edits) {
        Ok(fresh) => fresh,
        Err(reason) => return Ok(vec![reason]),
    };
    issues.extend(fresh.diagnostics);
    if fresh.baseline.source_census != b.source_census || fresh.baseline.native_sha256 != b.native_sha256 {
        issues.push("Original printable source census/native interpretation changed".into());
    }
    let Some(geometry) = request.geometry.as_ref() else {
        return Ok(vec!["Prepared geometry artifacts missing".into()]);
    };
    if fresh.geometry.regions.len() != geometry.regions.len() || fresh.baseline.selected_pages != b.selected_pages {
        issues.push("Prepared region/page census differs from the original and explicit edits".into());
    }
    for region in &b.regions {
        let captured = fresh.baseline.regions.iter().find(|r| r.id == region.id);
        if captured.is_none_or(|r| {
            r.prepared_mask_sha256 != region.prepared_mask_sha256
                || r.source_objects != region.source_objects
                || r.facts
                    .iter()
                    .any(|(key, fact)| region.facts.get(key).is_none_or(|stored| stored.value != fact.value || stored.provenance != fact.provenance))
        }) {
            issues.push(format!("{}: captured source identity or file facts were rewritten", region.id));
        }
        let actual = geometry.regions.get(&region.id);
        let original = fresh.geometry.regions.get(&region.id);
        if original.is_none() || actual.is_none() || digest(&actual)? != digest(&original)? {
            issues.push(format!("{}: intentional preparation edit needs separately verified transformation evidence", region.id));
        }
    }
    Ok(issues)
}

pub fn verify_into_report(request: &crate::Request, bytes: &[u8], report: &mut crate::Report) -> Result<(), String> {
    let issues = verify_original(request, bytes)?;
    report.record(
        "global/fidelity",
        crate::Status::Incomplete,
        if issues.is_empty() {
            "Original-to-baseline geometry verified; output paint and compositing fidelity still require qualification".into()
        } else {
            issues.join("; ")
        },
        serde_json::json!({"original_verified":issues.is_empty(),"issues":issues}),
        vec![Evidence {
            artifact_sha256: request.identity.original_sha256.clone(),
            producer: "checks/original-reread-1".into(),
            detail: "Fresh original decode and immutable region artifact comparison".into(),
        }],
    )?;
    crate::specification::finalize_placement(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_is_reproducible_unapproved_and_bound_to_original_bytes() {
        let bytes = crate::pdf::tests::pdf(true);
        let a = capture(&bytes, "capture".into(), "job".into()).unwrap();
        let b = capture(&bytes, "capture".into(), "job".into()).unwrap();
        assert_eq!(digest(&a).unwrap(), digest(&b).unwrap());
        assert!(a.baseline.approval.is_none());
        assert_eq!(a.geometry.regions.len(), 2);
        assert_eq!(a.previews.len(), 2);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let mut request = crate::attribution::tests::request();
        request.identity.original_sha256 = sha256(&bytes);
        request.baseline = Some(a.baseline);
        request.geometry = Some(a.geometry);
        assert!(verify_original(&request, &bytes).unwrap().is_empty());
        let original_order = request.geometry.as_ref().unwrap().regions.values().next().unwrap().source_order;
        request.geometry.as_mut().unwrap().regions.values_mut().next().unwrap().source_order = Some(999);
        assert!(!verify_original(&request, &bytes).unwrap().is_empty());
        request.geometry.as_mut().unwrap().regions.values_mut().next().unwrap().source_order = original_order;
        let original_facts = request.baseline.as_ref().unwrap().regions[0].facts.clone();
        request.baseline.as_mut().unwrap().regions[0].facts.get_mut("paint").unwrap().value = serde_json::json!("changed");
        assert!(verify_original(&request, &bytes).unwrap().iter().any(|i| i.contains("file facts")));
        request.baseline.as_mut().unwrap().regions[0].facts = original_facts;
        assert!(!verify_original(&request, b"different").unwrap().is_empty());
        request.geometry.as_mut().unwrap().regions.values_mut().next().unwrap().paint = serde_json::json!({"changed":true});
        assert!(!verify_original(&request, &bytes).unwrap().is_empty());
    }
}
