//! Explicit alpha-footprint preparation for bounded, fully accounted image-only originals.
use crate::{
    digest,
    preparation::{Fact, Provenance, Region},
    prepare::{Draft, Preview},
    prepared_mask::Mask,
    program::RegionGeometry,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use vectorcraft_geom::{Affine, FillRule, Rect, Shape};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub mask: Mask,
    pub paint: Value,
    pub ink: Option<String>,
}
fn diagnostic(id: &str) -> String {
    format!("{id}: explicit exact-binary-alpha boundary decision required")
}
/// Return false without altering the ordinary draft if this bounded path cannot account for it.
/// A raw clip flag needs retained exact containment evidence; it is never dropped.
pub fn capture(bytes: &[u8], draft: &mut Draft, native_images: usize, import_issues: &[String]) -> Result<bool, String> {
    const PAGE_METADATA_NOTE: &str = "original PDF page boxes are not retained for a rotated, scaled-unit or artwork-bounds import";
    if native_images > 1000 {
        return Err("Raster preparation source count exceeds the decision replay budget".into());
    }
    if native_images == 0 {
        return Ok(false);
    }
    if !draft.baseline.regions.is_empty() || !draft.geometry.regions.is_empty() {
        return Err("Existing prepared paint regions require mixed-source reconciliation and cannot be replaced by image masks".into());
    }
    if import_issues.iter().any(|s| s != PAGE_METADATA_NOTE) {
        return Err("Original import diagnostics prevent complete raster-source accounting".into());
    }
    let order = vectorcraft_pdf::print_order(bytes).map_err(|e| e.to_string())?;
    if order.pages.iter().any(|p| !p.is_empty())
        || order.images.iter().map(Vec::len).sum::<usize>() != native_images
        || order.issues.iter().any(|s| s != "Image sample, mask and compositing coverage requires independent reconciliation")
    {
        return Err("Raw image-only source census, paint count or graphics context could not be reconciled".into());
    }
    let info = vectorcraft_pdf::production_info(bytes).map_err(|e| e.to_string())?;
    if info.pages.len() != order.images.len() {
        return Err("Raw image pages differ from the production page inventory".into());
    }
    let mut sources = BTreeMap::new();
    let mut previews = BTreeMap::new();
    let mut pixels_total = 0usize;
    for (page, (metadata, images)) in info.pages.iter().zip(&order.images).enumerate() {
        let [mx0, my0, mx1, my1] = metadata.media_mm;
        let [cx0, cy0, cx1, cy1] = metadata.crop_mm;
        let [x0, y0, x1, y1] = [mx0.max(cx0), my0.max(cy0), mx1.min(cx1), my1.min(cy1)];
        if x0 >= x1 || y0 >= y1 {
            return Err("Raster source has an empty physical page crop".into());
        }
        let page_xf = match metadata.rotation {
            0 => Affine::new([1.0, 0.0, 0.0, -1.0, -x0, y1]),
            90 => Affine::new([0.0, 1.0, 1.0, 0.0, -y0, -x0]),
            180 => Affine::new([-1.0, 0.0, 0.0, 1.0, x1, -y0]),
            270 => Affine::new([0.0, -1.0, -1.0, 0.0, y1, x1]),
            _ => return Err("Raster source page rotation is unsupported".into()),
        } * Affine::scale(metadata.user_unit * 25.4 / 72.0);
        let size = if matches!(metadata.rotation, 90 | 270) { [y1 - y0, x1 - x0] } else { [x1 - x0, y1 - y0] };
        let page_bounds = Rect::new(0.0, 0.0, size[0], size[1]);
        for (index, image) in images.iter().enumerate() {
            if image.clip_context && image.containing_form_clip.is_none() {
                return Err("Clipped raster preparation requires a retained and reviewed mask/clip boundary".into());
            }
            if image.overprint || image.compositing_unresolved || image.description.optional_content || image.description.interpolate {
                return Err("Raster source overprint, compositing, optional content or interpolation remains unresolved".into());
            }
            let samples =
                image.samples.as_ref().ok_or_else(|| image.sample_error.clone().unwrap_or("Raster source samples are unavailable".into()))?;
            let matrix = image.matrix.ok_or("Raster source transform is unavailable")?;
            let count = (samples.width as usize).checked_mul(samples.height as usize).ok_or("Raster preparation dimensions overflow")?;
            pixels_total = pixels_total.saturating_add(count);
            if count == 0 || pixels_total > 1_048_576 {
                return Err("Raster preparation exceeds the cumulative 1,048,576-pixel budget or has empty dimensions".into());
            }
            let mut bits = vec![0u8; count.div_ceil(8)];
            for at in 0..count {
                let alpha = samples.alpha(at)?;
                if alpha == 1.0 {
                    if let Some(byte) = bits.get_mut(at / 8) {
                        *byte |= 1 << (7 - at % 8);
                    }
                } else if alpha != 0.0 {
                    return Err("Fractional image alpha requires a separately reviewed boundary policy; no threshold is inferred".into());
                }
            }
            let transform =
                page_xf * Affine::new(matrix) * Affine::new([1.0 / f64::from(samples.width), 0.0, 0.0, -1.0 / f64::from(samples.height), 0.0, 1.0]);
            // Cropping the image needs a separately retained clip; the initial subset refuses it.
            let bounds = transform.transform_rect_bbox(Rect::new(0.0, 0.0, f64::from(samples.width), f64::from(samples.height)));
            if bounds.x0 < page_bounds.x0 || bounds.y0 < page_bounds.y0 || bounds.x1 > page_bounds.x1 || bounds.y1 > page_bounds.y1 {
                return Err("Raster source extends outside the physical page crop; explicit clipping preparation is required".into());
            }
            let mask = Mask {
                source_sha256: digest(image)?,
                width: samples.width,
                height: samples.height,
                page,
                pixel_to_page_mm: transform.as_coeffs(),
                bits_hex: bits.iter().map(|b| format!("{b:02x}")).collect(),
            };
            let path = mask.path()?;
            let b = path.to_bezpath().bounding_box();
            let id = format!("page-{page}-image-{index}");
            previews.insert(id.clone(), Preview { page, bounds_mm: [b.x0, b.y0, b.x1, b.y1], path_svg: path.to_bezpath().to_svg() });
            let ink = (image.description.colour_space == "Separation" && samples.colourants.len() == 1)
                .then(|| samples.colourants.first().cloned())
                .flatten();
            let visible_sample =
                if samples.process_base().is_some() { crate::raster_fidelity::visible_signature(&mask, samples)? } else { Value::Null };
            let mut paint = json!({"visible_sample":visible_sample,"type":"source_image","source_sha256":mask.source_sha256,"sample_sha256":samples.sample_sha256,"colourants":samples.colourants,"description":image.description});
            if image.description.colour_space == "Indexed"
                && let Some(base) = samples.process_base()
            {
                paint["indexed_process_base"] = json!(base);
            }
            sources.insert(id, Source { mask, paint, ink });
        }
    }
    if sources.is_empty() {
        return Ok(false);
    }
    // Source geometry above uses complete raw page metadata, independently of editable
    // artboard retention. Other import diagnostics have already refused this path.
    draft.notices.extend(import_issues.iter().cloned());
    draft.baseline.source_census = sources.keys().cloned().collect();
    draft.baseline.regions.clear();
    draft.geometry.regions.clear();
    draft.diagnostics = sources.keys().map(|id| diagnostic(id)).collect();
    draft.previews = previews;
    draft.raster_sources = sources;
    draft.baseline.census_evidence.producer = "checks/raw-image-preparation-1".into();
    draft.baseline.census_evidence.detail =
        "Complete bounded image-only raw census; alpha-footprint candidates require explicit preparation decisions".into();
    Ok(true)
}
pub fn apply(draft: &mut Draft, id: &str, source_sha256: &str, policy: &str) -> Result<(), String> {
    if policy != "exact_binary_alpha" {
        return Err("Unsupported raster boundary policy".into());
    }
    let source = draft.raster_sources.get(id).ok_or("Raster preparation source missing")?;
    if source.mask.source_sha256 != source_sha256 || draft.geometry.regions.contains_key(id) {
        return Err("Raster source changed or mask decision duplicated".into());
    }
    let artifact = RegionGeometry {
        page: source.mask.page,
        path_mm: source.mask.path()?,
        fill_rule: FillRule::NonZero,
        paint: source.paint.clone(),
        geometry_error_mm: source.mask.geometry_error_mm(),
        source_order: None,
        prepared_mask: Some(source.mask.clone()),
    };
    let provenance = Provenance::File { artifact_sha256: draft.baseline.original_sha256.clone(), locator: id.into() };
    let mut facts = BTreeMap::from([("source_kind".into(), Fact { value: json!("image"), provenance: provenance.clone() })]);
    // Keep the paint predicate's existing meaning (source colour model), not object kind.
    // Indexed bases use the retained decoded model, never a guess from colourant names.
    let model = match crate::raster_fidelity::process_model(&source.paint) {
        Some("DeviceCMYK") => Some("cmyk"),
        Some("DeviceGray") => Some("gray"),
        _ => None,
    };
    if let Some(model) = model {
        facts.insert("paint".into(), Fact { value: json!(model), provenance: provenance.clone() });
    }
    if let Some(ink) = &source.ink {
        facts.insert("ink".into(), Fact { value: json!(ink), provenance });
    }
    draft.baseline.regions.push(Region {
        id: id.into(),
        source_objects: vec![id.into()],
        geometry_sha256: digest(&artifact)?,
        facts,
        prepared_mask_sha256: Some(digest(&source.mask)?),
    });
    draft.geometry.regions.insert(id.into(), artifact);
    let pending = diagnostic(id);
    draft.diagnostics.retain(|s| s != &pending);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(alpha: &str) -> Vec<u8> {
        image_in(alpha, "", "q 36 0 0 36 18 18 cm /Im Do Q")
    }
    fn image_in(alpha: &str, page_entries: &str, content: &str) -> Vec<u8> {
        crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] {page_entries} /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>"),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /XObject /Subtype /Image /Width 3 /Height 3 /BitsPerComponent 8 /ColorSpace /DeviceGray /SMask 6 0 R /Filter /ASCIIHexDecode /Length 19 >>\nstream\n000000000000000000>\nendstream".into(),
            format!("<< /Type /XObject /Subtype /Image /Width 3 /Height 3 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode /Length {} >>\nstream\n{alpha}>\nendstream", alpha.len()+1),
        ])
    }

    fn decision(id: &str, hash: &str) -> Fact {
        Fact {
            value: json!({"kind":"raster_mask","source":id,"source_sha256":hash,"policy":"exact_binary_alpha"}),
            provenance: Provenance::Operator {
                actor: "Synthetic test".into(),
                at: "2026-10-19".into(),
                decision_id: "Reviewed alpha-footprint test".into(),
                evidence_sha256: "e".repeat(64),
            },
        }
    }
    #[test]
    fn captured_alpha_boundary_requires_a_decision_and_fresh_original_replay() {
        let bytes = image("ffffffff00ffffffff");
        let capture = crate::prepare::capture(&bytes, "capture".into(), "job".into()).unwrap();
        assert_eq!(capture.raster_sources.len(), 1, "{:?}", capture.diagnostics);
        assert!(capture.baseline.regions.is_empty());
        assert!(!capture.diagnostics.is_empty());
        let (id, source) = capture.raster_sources.first_key_value().unwrap();
        let edits = vec![decision(id, &source.mask.source_sha256)];
        let prepared = crate::prepare_edits::replay(capture.clone(), &edits).unwrap();
        assert!(prepared.diagnostics.is_empty(), "{:?}", prepared.diagnostics);
        assert_eq!(prepared.baseline.source_census, capture.baseline.source_census);
        assert!(prepared.baseline.regions[0].prepared_mask_sha256.is_some());
        assert_eq!(prepared.geometry.regions[id].paint["type"], "source_image");
        let mut request = crate::attribution::tests::request();
        request.identity.original_sha256 = crate::sha256(&bytes);
        request.baseline = Some(prepared.baseline);
        request.geometry = Some(prepared.geometry);
        assert!(crate::prepare::verify_original(&request, &bytes).unwrap().is_empty());
        request.geometry.as_mut().unwrap().regions.get_mut(id).unwrap().prepared_mask.as_mut().unwrap().bits_hex = "ff80".into();
        assert!(!crate::prepare::verify_original(&request, &bytes).unwrap().is_empty());
        assert!(crate::prepare_edits::replay(capture.clone(), &[decision(id, &"b".repeat(64))]).is_err());
        assert!(crate::prepare_edits::replay(capture, &[edits[0].clone(), edits[0].clone()]).is_err());
    }
    #[test]
    fn fractional_alpha_is_not_thresholded_into_a_prepared_boundary() {
        let capture = crate::prepare::capture(&image("ffffffff80ffffffff"), "capture".into(), "job".into()).unwrap();
        assert!(capture.raster_sources.is_empty());
        assert!(!capture.diagnostics.is_empty());
    }
    #[test]
    fn existing_prepared_regions_cannot_be_replaced_by_a_new_raster_census() {
        let bytes = image("ffffffff00ffffffff");
        let mut draft = crate::prepare::capture(&bytes, "capture".into(), "job".into()).unwrap();
        let (id, source) = draft.raster_sources.first_key_value().unwrap();
        let (id, hash) = (id.clone(), source.mask.source_sha256.clone());
        apply(&mut draft, &id, &hash, "exact_binary_alpha").unwrap();
        let before = serde_json::to_value(&draft).unwrap();
        assert!(capture(&bytes, &mut draft, 1, &[]).unwrap_err().contains("cannot be replaced"));
        assert_eq!(serde_json::to_value(&draft).unwrap(), before);
    }
    #[test]
    fn raw_page_rotation_units_and_mirroring_place_exact_mask_footprints() {
        for (rotation, expected) in
            [(0, [6.0, 18.0, 30.0, 54.0]), (90, [18.0, 6.0, 54.0, 30.0]), (180, [42.0, 18.0, 66.0, 54.0]), (270, [18.0, 42.0, 54.0, 66.0])]
        {
            for unit in [1.0, 2.0] {
                let bytes = image_in("ffffffffffffffffff", &format!("/Rotate {rotation} /UserUnit {unit}"), "q 24 0 0 36 6 18 cm /Im Do Q");
                let capture = crate::prepare::capture(&bytes, "capture".into(), "job".into()).unwrap();
                assert_eq!(capture.raster_sources.len(), 1, "{rotation}/{unit}: {:?}", capture.diagnostics);
                let b = capture.previews.values().next().unwrap().bounds_mm;
                for (a, e) in b.into_iter().zip(expected) {
                    assert!((a - e * unit * 25.4 / 72.0).abs() < 0.00002, "{rotation}/{unit}: {b:?}");
                }
            }
        }
        let bytes = image_in("ff0000000000000000", "", "q -36 0 0 36 54 18 cm /Im Do Q");
        let capture = crate::prepare::capture(&bytes, "capture".into(), "job".into()).unwrap();
        let b = capture.previews.values().next().unwrap().bounds_mm;
        for (a, e) in b.into_iter().zip([42.0, 18.0, 54.0, 30.0]) {
            assert!((a - e * 25.4 / 72.0).abs() < 0.00002);
        }
    }
    #[test]
    fn explicit_crop_and_affine_edits_replay_after_mask_selection() {
        let bytes = image("ffffffff00ffffffff");
        let capture = crate::prepare::capture(&bytes, "capture".into(), "job".into()).unwrap();
        let (id, source) = capture.raster_sources.first_key_value().unwrap();
        let mut crop = decision(id, &source.mask.source_sha256);
        crop.value = json!({"kind":"crop","regions":[id],"bounds_mm":[6.35,6.35,12.7,19.05]});
        let mut affine = crop.clone();
        affine.value = json!({"kind":"affine","regions":[id],"matrix":[-2,0,0,2,50,5],"target_page":1});
        let prepared = crate::prepare_edits::replay(capture.clone(), &[decision(id, &source.mask.source_sha256), crop, affine]).unwrap();
        assert_eq!(prepared.baseline.selected_pages, vec![1]);
        assert_eq!(prepared.geometry.regions[id].page, 1);
        assert_eq!(prepared.geometry.regions[id].prepared_mask.as_ref().unwrap().page, 0);
        assert_eq!(prepared.geometry.regions[id].paint, source.paint);
        let mut request = crate::attribution::tests::request();
        request.identity.original_sha256 = crate::sha256(&bytes);
        request.baseline = Some(prepared.baseline);
        request.geometry = Some(prepared.geometry);
        assert!(crate::prepare::verify_original(&request, &bytes).unwrap().is_empty());
        request.baseline.as_mut().unwrap().intentional_edits.remove(0);
        assert!(!crate::prepare::verify_original(&request, &bytes).unwrap().is_empty());
    }
    #[test]
    fn unsupported_clipping_mixed_content_and_unknown_policy_stay_unresolved() {
        for content in ["0 0 72 72 re W n q 36 0 0 36 18 18 cm /Im Do Q", "q 36 0 0 36 18 18 cm /Im Do Q 0 0 1 1 re f"] {
            let capture = crate::prepare::capture(&image_in("ffffffff00ffffffff", "", content), "capture".into(), "job".into()).unwrap();
            assert!(capture.raster_sources.is_empty());
            assert!(!capture.diagnostics.is_empty());
        }
        let capture = crate::prepare::capture(&image("ffffffff00ffffffff"), "capture".into(), "job".into()).unwrap();
        let (id, source) = capture.raster_sources.first_key_value().unwrap();
        let mut edit = decision(id, &source.mask.source_sha256);
        edit.value["policy"] = json!("infer_nonwhite_pixels");
        assert!(crate::prepare_edits::replay(capture, &[edit]).is_err());
    }
    #[test]
    fn invalid_image_declarations_cannot_become_preparation_sources() {
        for (layout, filter, data, eligible) in [
            ("/Width 1 /Height 1 /BitsPerComponent 8", "", "x", true),
            ("/Width 1 /Height 1 /BitsPerComponent 8", "/Filter /ASCIIHexDecode", "78>", true),
            ("/Width 1 /Height 1 /BitsPerComponent 8", "/Filter /UnknownCodec", "x", false),
            ("/Width 1 /Height 1 /BitsPerComponent 8", "/Filter 42", "x", false),
            ("/Width 1 /Height 1 /BitsPerComponent 8", "/DecodeParms 42", "x", false),
            ("/Width 1.5 /Height 1 /BitsPerComponent 8", "", "x", false),
            ("/Width 1 /Height 1.5 /BitsPerComponent 8", "", "x", false),
            ("/Width 1 /Height 1 /BitsPerComponent 8.5", "", "x", false),
        ] {
            let bytes = crate::pdf::tests::objects_pdf(&[
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>".into(),
                "<< /Length 6 >>\nstream\n/Im Do\nendstream".into(),
                format!(
                    "<< /Type /XObject /Subtype /Image {layout} /ColorSpace /DeviceGray {filter} /Length {} >>\nstream\n{data}\nendstream",
                    data.len()
                ),
            ]);
            let capture = crate::prepare::capture(&bytes, "capture".into(), "job".into()).unwrap();
            assert_eq!(!capture.raster_sources.is_empty(), eligible, "{layout} {filter}");
            assert!(!capture.diagnostics.is_empty());
        }
    }
    #[test]
    fn original_synthetic_mask_preparation_cases_are_retained_for_cross_runtime_checks() {
        let Ok(directory) = std::env::var("CHECK_PREP_MASK_EVIDENCE_DIR") else {
            return;
        };
        let directory = std::path::Path::new(&directory);
        std::fs::create_dir_all(directory).unwrap();
        let cases = [
            ("ring", image("ffffffff00ffffffff"), true),
            ("opaque", image("ffffffffffffffffff"), true),
            ("empty", image("000000000000000000"), true),
            ("fractional", image("ffffffff80ffffffff"), false),
            ("clip", image_in("ffffffff00ffffffff", "", "0 0 72 72 re W n q 36 0 0 36 18 18 cm /Im Do Q"), false),
            ("mixed", image_in("ffffffff00ffffffff", "", "q 36 0 0 36 18 18 cm /Im Do Q 0 0 1 1 re f"), false),
            ("rotate-unit", image_in("ffffffff00ffffffff", "/Rotate 90 /UserUnit 2", "q 24 0 0 36 6 18 cm /Im Do Q"), true),
            ("mirror", image_in("ff0000000000000000", "", "q -36 0 0 36 54 18 cm /Im Do Q"), true),
        ];
        let mut manifest = vec![];
        for (name, bytes, eligible) in cases {
            std::fs::write(directory.join(format!("{name}.pdf")), &bytes).unwrap();
            let capture = crate::prepare::capture(&bytes, "00000000-0000-4000-8000-000000000008".into(), "synthetic-raster-job".into()).unwrap();
            assert_eq!(!capture.raster_sources.is_empty(), eligible, "{name}");
            manifest.push(json!({"name":name,"eligible":eligible,"pdf_sha256":crate::sha256(&bytes)}));
            if eligible {
                let (id, source) = capture.raster_sources.first_key_value().unwrap();
                let mut edits = vec![decision(id, &source.mask.source_sha256)];
                std::fs::write(directory.join(format!("{name}-edits.json")), serde_json::to_vec(&edits).unwrap()).unwrap();
                if name == "ring" {
                    let mut affine = edits[0].clone();
                    affine.value = json!({"kind":"affine","regions":[id],"matrix":[-2,0,0,2,50,5],"target_page":1});
                    edits.push(affine);
                    std::fs::write(directory.join("ring-affine-edits.json"), serde_json::to_vec(&edits).unwrap()).unwrap();
                }
            }
        }
        std::fs::write(directory.join("manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }
}

#[cfg(test)]
#[path = "raster_prepare_clip.rs"]
mod clip_tests;
