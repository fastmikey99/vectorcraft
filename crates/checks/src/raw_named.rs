//! Opaque named-colour prediction for the independent interpreter comparison.
//! The editable RGB preview is never used as a substitute for physical colourants.
use crate::pdf::PaintedObject;
use std::collections::{BTreeMap, BTreeSet};
use vectorcraft_pdf::{PrintColorSpace, PrintPaintSource};

pub(crate) const PREVIEW_NOTICE: &str = "colours mixing several inks (DeviceN) were imported as RGB";

/// This is deliberately separate from source_tones used by manufacturing checks.
/// Raw named values can predict physical plates without establishing editable
/// source fidelity, contribution ownership, or the meaning of the RGB preview.
pub(crate) fn prediction_tones(paint: &PaintedObject, source: &PrintPaintSource) -> Result<BTreeMap<String, f64>, String> {
    if source.devicen_attributes {
        return Err("DeviceN attributes require separate physical prediction qualification".into());
    }
    if source.color_space != PrintColorSpace::DeviceN || source.colorants.len() <= 1 {
        return crate::tone::source_tones(paint, source);
    }
    if source.colorants.len() > 32
        || source.colorants.iter().any(|name| name.is_empty() || matches!(name.as_str(), "All" | "None"))
        || source.colorants.iter().collect::<BTreeSet<_>>().len() != source.colorants.len()
    {
        return Err("Raw DeviceN prediction requires distinct resolved physical colourants".into());
    }
    let components = source.components.as_ref().ok_or("Raw DeviceN components missing")?;
    if components.len() != source.colorants.len() || components.iter().any(|value| !value.is_finite() || !(0.0..=1.0).contains(value)) {
        return Err("Raw DeviceN components are incomplete or outside the qualified unit interval".into());
    }
    Ok(source.colorants.iter().cloned().zip(components.iter().copied()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Status,
        independent::{Configuration, render},
    };
    fn fixture(names: &str, content: &str, attributes: &str) -> Vec<u8> {
        let function = "{ 0 0 }";
        let form = "/D cs 0.25 0.75 scn 10 10 40 40 re f";
        crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Resources << /ColorSpace << /D [/DeviceN [{names}] /DeviceCMYK 5 0 R {attributes}] >> /ExtGState << /OP0 << /OP true /op true /OPM 0 >> /OP1 << /OP true /op true /OPM 1 >> /KO << /OP false /op false /OPM 1 >> >> /XObject << /Fm 6 0 R >> >> /Contents 4 0 R >>"
            ),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            format!("<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length {} >>\nstream\n{function}\nendstream", function.len()),
            format!(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 72 72] /Resources << /ColorSpace << /D [/DeviceN [{names}] /DeviceCMYK 5 0 R {attributes}] >> >> /Length {} >>\nstream\n{form}\nendstream",
                form.len()
            ),
        ])
    }
    fn prediction(bytes: &[u8]) -> crate::pdf::Census {
        let imported = vectorcraft_pdf::import_with_report(
            bytes,
            &vectorcraft_pdf::ImportOptions {
                preserve_clip_paths: true,
                reject_depth_loss: true,
                text_as: vectorcraft_pdf::TextAs::Outlines,
                ..Default::default()
            },
        )
        .unwrap();
        let mut c = crate::pdf::census(&imported.document);
        c.unresolved.extend(crate::pdf::import_messages(imported.warnings).0.into_iter().filter(|w| w != PREVIEW_NOTICE));
        crate::ordering::reconcile(bytes, &mut c);
        c
    }
    #[test]
    fn raw_named_components_do_not_waive_unsupported_names_or_attributes() {
        for (names, attributes) in [("/One /None", ""), ("/One /All", ""), ("/One /One", "")] {
            let c = prediction(&fixture(names, "/D cs 0.25 0.75 scn 10 10 40 40 re f", attributes));
            assert!(!c.paint_order_verified, "{names} {attributes}");
        }
        let attributed = prediction(&fixture("/One /Two", "/D cs 0.25 0.75 scn 10 10 40 40 re f", "<< /Subtype /NChannel >>"));
        assert!(attributed.paint_order_verified, "Attributes must not invalidate structural order: {:?}", attributed.unresolved);
        let painted = &attributed.paints[0];
        let attributed_source = &attributed.paint_sources[painted.source_space.unwrap()];
        assert!(attributed_source.devicen_attributes);
        assert!(prediction_tones(painted, attributed_source).unwrap_err().contains("attributes"));
        let process = prediction(&fixture("/Cyan /Magenta", "/D cs 0.25 0.75 scn 10 10 40 40 re f", "<< /Subtype /NChannel >>"));
        assert!(process.paint_order_verified);
        let p = &process.paints[0];
        let source = &process.paint_sources[p.source_space.unwrap()];
        assert!(crate::tone::source_tones(p, source).is_ok(), "Existing raw/decoded structural facets remain available");
        assert!(prediction_tones(p, source).is_err());
        let c = prediction(&fixture("/One /Two", "/D cs 0.25 0.75 scn 10 10 40 40 re f", ""));
        assert!(c.paint_order_verified, "{:?}", c.unresolved);
        let p = c.paints.last().unwrap();
        let source = &c.paint_sources[p.source_space.unwrap()];
        assert_eq!(prediction_tones(p, source).unwrap(), BTreeMap::from([("One".into(), 0.25), ("Two".into(), 0.75)]));
        assert!(crate::tone::source_tones(p, source).is_err());
        for components in [None, Some(vec![0.25]), Some(vec![0.25, f64::NAN]), Some(vec![0.25, 2.0])] {
            let mut bad = source.clone();
            bad.components = components;
            assert!(prediction_tones(p, &bad).is_err());
        }
    }
    #[test]
    fn full_verification_uses_raw_named_prediction_without_waiving_report_gaps() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else { return };
        let bytes = fixture("/One /Two", "/D cs 0.25 0.75 scn 10 10 40 40 re f", "");
        let mut request = crate::tests::request();
        request.identity.pdf_sha256 = crate::sha256(&bytes);
        request.identity.recipe_sha256 = crate::digest(&request.recipe).unwrap();
        request.acceptance.insert(
            "geometry_tolerance_mm".into(),
            crate::preparation::Fact {
                value: serde_json::json!(0.6),
                provenance: crate::preparation::Provenance::Operator {
                    actor: "Synthetic fixture".into(),
                    at: "2026-10-20".into(),
                    decision_id: "Coarse-pixel test only; not manufacturing acceptance".into(),
                    evidence_sha256: "e".repeat(64),
                },
            },
        );
        request.identity.measurement_sha256 = crate::digest(&request.acceptance).unwrap();
        let baseline = crate::pdf::check(&request, &bytes).unwrap();
        let config = Configuration {
            binary: binary.clone().into(),
            binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
            version: "10.07.1".into(),
            dpi: 72,
            timeout_ms: 10_000,
            max_output_bytes: 16 * 1024 * 1024,
            qualification_sha256: None,
        };
        let base = std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let output =
            base.join(format!("named-verification-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let result = crate::independent::verify(&request, &bytes, &config, &output).unwrap();
        assert_eq!(result.comparisons.len(), 2);
        assert!(result.comparisons.iter().all(|c| c.status == Status::Pass), "{:?}", result.comparisons);
        assert_eq!(result.report.coverage.total, baseline.coverage.total);
        for before in baseline.findings.iter().filter(|f| f.obligation.key != "global/independent") {
            assert_eq!(result.report.findings.iter().find(|f| f.obligation.key == before.obligation.key).unwrap().status, before.status);
        }
        assert_eq!(result.report.findings.iter().find(|f| f.obligation.key == "global/independent").unwrap().status, Status::Incomplete);
        assert_ne!(result.report.status, Status::Pass);
        std::fs::write(output.join("verification.json"), serde_json::to_vec_pretty(&result).unwrap()).unwrap();
        let attributed = fixture("/One /Two", "/D cs 0.25 0.75 scn 10 10 40 40 re f", "<< /Subtype /NChannel >>");
        request.identity.pdf_sha256 = crate::sha256(&attributed);
        let refusal_output =
            base.join(format!("attribute-verification-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let refused = crate::independent::verify(&request, &attributed, &config, &refusal_output).unwrap();
        assert_eq!(refused.comparisons.len(), 2);
        assert!(refused.comparisons.iter().all(|c| c.status == Status::Incomplete && c.issues.iter().any(|issue| issue.contains("attributes"))));
        assert_eq!(refused.report.coverage.total, result.report.coverage.total);
        std::fs::write(refusal_output.join("verification.json"), serde_json::to_vec_pretty(&refused).unwrap()).unwrap();
    }
    #[test]
    fn independent_named_planes_match_raw_tints_and_detect_changed_pixels() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else { return };
        let base = std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let mut cases = vec![];
        for mode in ["OP0", "OP1", "KO"] {
            for (label, names, values) in [
                ("two-spots", "/One /Two", "0.25 0.75"),
                ("zero-spot", "/One /Two", "0 1"),
                ("zero-process", "/One /Cyan", "0.25 0"),
                ("renamed-process", "/Custom#20ink /Cyan", "0.25 0.5"),
            ] {
                cases.push((
                    format!("{label}-{mode}"),
                    fixture(names, &format!("1 1 1 0 k 0 0 72 72 re f /{mode} gs /D cs {values} scn 10 10 40 40 re f"), ""),
                ));
            }
        }
        for mode in ["OP0", "OP1", "KO"] {
            cases.push((
                format!("clear-existing-spot-{mode}"),
                fixture("/One /Two", &format!("/D cs 1 1 scn 0 0 72 72 re f /{mode} gs 0 1 scn 10 10 40 40 re f"), ""),
            ));
        }
        cases.push(("clipped".into(), fixture("/One /Two", "q 10 10 10 40 re W n /D cs 0.25 0.75 scn 10 10 40 40 re f Q", "")));
        cases.push(("form".into(), fixture("/One /Two", "/Fm Do", "")));
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        for dpi in [72, 144] {
            for (label, bytes) in &cases {
                let c = prediction(bytes);
                assert!(c.paint_order_verified, "{label}: {:?}", c.unresolved);
                let output = base.join(format!("named-prediction-{stamp}-{label}-{dpi}"));
                let config = Configuration {
                    binary: binary.clone().into(),
                    binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                    version: "10.07.1".into(),
                    dpi,
                    timeout_ms: 10_000,
                    max_output_bytes: 16 * 1024 * 1024,
                    qualification_sha256: None,
                };
                let rendering = render(&config, bytes, &output).unwrap();
                let comparison = crate::raster::compare_named(&c, &rendering, Some(0.6)).unwrap();
                assert_eq!(comparison.status, Status::Pass, "{label}/{dpi}: {comparison:?}");
                assert_eq!(crate::raster::compare(&c, &rendering, Some(0.6)).unwrap().status, Status::Incomplete);
                let mut changed = rendering.clone();
                let plane = changed
                    .planes
                    .iter_mut()
                    .find(|p| !["Cyan", "Magenta", "Yellow", "Black"].contains(&p.name.as_str()) && p.minimum < 255)
                    .unwrap();
                let mut image = crate::raster::read_plane(plane).unwrap();
                let x = 15 * dpi / 72;
                let y = image.height() - 20 * dpi / 72;
                image.put_pixel(x, y, image::Luma([255 - image.get_pixel(x, y).0[0]]));
                plane.path = output.join("changed-plane.tif");
                image.save(&plane.path).unwrap();
                plane.sha256 = crate::sha256(&std::fs::read(&plane.path).unwrap());
                let negative = crate::raster::compare_named(&c, &changed, Some(0.6)).unwrap();
                assert_eq!(negative.status, Status::Fail, "{label}/{dpi}: {negative:?}");
                std::fs::write(output.join("qualification.json"), serde_json::to_vec_pretty(&serde_json::json!({"engine_version":crate::ENGINE_VERSION,"scope":"raw-named-physical-prediction-only","positive":comparison,"changed_pixel":negative,"render":rendering})).unwrap()).unwrap();
            }
        }
    }
}
