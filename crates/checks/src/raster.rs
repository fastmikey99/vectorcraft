//! Independent per-ink tonal reconciliation. No binary zero/nonzero mask or area suppression.
//! Solid device CMYK/grey and named-spot paints are predicted in paint order. Unsupported
//! source colour spaces, groups, images and masks remain explicit rather than RGB-flattened.
use crate::{Status, geometry::Boundary, independent::Render, pdf::Census};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use vectorcraft_geom::{FillRule, Point};
#[derive(Clone, Debug, Serialize)]
pub struct PlaneComparison {
    pub page: u32,
    pub name: String,
    pub status: Status,
    pub compared_pixels: u64,
    pub boundary_pixels: u64,
    pub wrong_pixels: u64,
    pub maximum_tone_error: f64,
    pub first_defect_mm: Option<[f64; 2]>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Comparison {
    pub status: Status,
    pub uncertainty_mm: f64,
    pub planes: Vec<PlaneComparison>,
    pub issues: Vec<String>,
}
/// Bound all retained segment copies, including both spatial indexes.
pub(crate) fn retain_boundary(boundary: &Boundary, retained: &mut usize) -> Result<(), String> {
    *retained = retained.saturating_add(boundary.retained_segments());
    if *retained > 1_000_000 {
        return Err("Independent aggregate geometry storage budget exceeded".into());
    }
    Ok(())
}
/// An interrupted measurement retains confirmed defects and the measured counts.
/// Unvisited pixels/planes can never turn a partial result into Pass.
pub(crate) fn interrupted(mut planes: Vec<PlaneComparison>, current: Option<PlaneComparison>, uncertainty_mm: f64, reason: String) -> Comparison {
    if let Some(mut current) = current {
        current.status = if current.wrong_pixels > 0 { Status::Fail } else { Status::Incomplete };
        planes.push(current);
    }
    let status = if planes.iter().any(|p| p.status == Status::Fail) { Status::Fail } else { Status::Incomplete };
    Comparison { status, uncertainty_mm, planes, issues: vec![reason] }
}
/// Callers preflight the total pixel budget before decoding a plane.
pub(crate) fn read_plane(plane: &crate::independent::Plane) -> Result<image::GrayImage, String> {
    let bytes = std::fs::read(&plane.path).map_err(|e| e.to_string())?;
    if crate::sha256(&bytes) != plane.sha256 {
        return Err("Independent plane changed before measurement".into());
    }
    let dimensions = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    if dimensions != (plane.width, plane.height) {
        return Err("Independent plane dimensions changed".into());
    }
    let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?.into_luma8();
    if image.dimensions() != (plane.width, plane.height) {
        return Err("Independent plane dimensions changed".into());
    }
    Ok(image)
}
struct Paint {
    page: usize,
    boundary: Boundary,
    inks: BTreeMap<String, f64>,
    overprint: bool,
    skip_zero_process: bool,
}
pub(crate) use crate::tone::source_tones;
pub fn compare(census: &Census, render: &Render, tolerance_mm: Option<f64>) -> Result<Comparison, String> {
    let uncertainty_mm = render.pixel_pitch_mm * 2.0f64.sqrt() + 0.000002 + census.paints.iter().map(|p| p.geometry_error_mm).fold(0.0, f64::max);
    let mut issues = census.unresolved.clone();
    if !census.paint_order_verified {
        issues.push("Raw PDF paint order and source spaces have not been reconciled".into());
    }
    issues.extend(render.diagnostics.clone());
    if census.budget_exhausted {
        issues.push("Native census budget exhausted".into());
    }
    if tolerance_mm.is_none_or(|t| !t.is_finite() || t < uncertainty_mm) {
        issues.push("Independent raster resolution does not resolve approved geometric acceptance".into());
    }
    let mut paints = vec![];
    let mut retained_segments = 0usize;
    let mut inks: BTreeSet<String> = ["Cyan", "Magenta", "Yellow", "Black"].into_iter().map(String::from).collect();
    for p in &census.paints {
        let (Some(page), Some(path)) = (p.page, p.geometry_mm.as_ref()) else {
            issues.push(format!("{}: full print coverage unavailable", p.id));
            continue;
        };
        let Some(source) = p.source_space.and_then(|i| census.paint_sources.get(i)) else {
            issues.push(format!("{}: raw PDF source colour space is unavailable", p.id));
            continue;
        };
        match (Boundary::new(path, FillRule::NonZero, 0.000001), source_tones(p, source)) {
            (Ok(boundary), Ok(tones)) => {
                if let Err(reason) = retain_boundary(&boundary, &mut retained_segments) {
                    issues.push(reason);
                    return Ok(Comparison { status: Status::Incomplete, uncertainty_mm, planes: vec![], issues });
                }
                inks.extend(tones.keys().cloned());
                paints.push(Paint {
                    page,
                    boundary,
                    inks: tones,
                    overprint: p.overprint,
                    skip_zero_process: crate::tone::skip_zero_process(p, source),
                });
            }
            (a, b) => {
                if let Err(e) = a {
                    issues.push(e);
                }
                if let Err(e) = b {
                    issues.push(e);
                }
            }
        }
    }
    let inventory: BTreeSet<_> = render.planes.iter().map(|p| (p.page, p.name.clone())).collect();
    if inventory.len() != render.planes.len() {
        issues.push("Duplicate independent colourant planes".into());
    }
    for page in 0..census.pages {
        for name in &inks {
            if !inventory.contains(&(page as u32 + 1, name.clone())) {
                issues.push(format!("Page {}: missing {name:?} plane", page + 1));
            }
        }
    }
    for p in &render.planes {
        if p.page == 0 || p.page as usize > census.pages || !inks.contains(&p.name) {
            issues.push(format!("Unexpected page/ink: {} {:?}", p.page, p.name));
        }
        if let Some([w, h]) = p.page.checked_sub(1).and_then(|n| census.page_sizes_mm.get(n as usize))
            && ((w / render.pixel_pitch_mm - f64::from(p.width)).abs() > 1.0 || (h / render.pixel_pitch_mm - f64::from(p.height)).abs() > 1.0)
        {
            issues.push(format!("Page {}: independent crop/scale dimensions disagree", p.page));
        }
    }
    if !issues.is_empty() {
        return Ok(Comparison { status: Status::Incomplete, uncertainty_mm, planes: vec![], issues });
    }
    let mut results = vec![];
    let mut budget = 0usize;
    let mut pixels = 0u64;
    for p in &render.planes {
        let count = u64::from(p.width) * u64::from(p.height);
        if count == 0 || count > 20_000_000 - pixels {
            return Ok(interrupted(results, None, uncertainty_mm, "Independent tonal comparison pixel budget exceeded".into()));
        }
        let image = match read_plane(p) {
            Ok(image) => image,
            Err(reason) => return Ok(interrupted(results, None, uncertainty_mm, reason)),
        };
        let mut result = PlaneComparison {
            page: p.page,
            name: p.name.clone(),
            status: Status::Pass,
            compared_pixels: 0,
            boundary_pixels: 0,
            wrong_pixels: 0,
            maximum_tone_error: 0.0,
            first_defect_mm: None,
        };
        for (x, y, pixel) in image.enumerate_pixels() {
            pixels += 1;
            if pixels > 20_000_000 {
                return Ok(interrupted(results, Some(result), uncertainty_mm, "Independent tonal comparison pixel budget exceeded".into()));
            }
            let point = Point::new((x as f64 + 0.5) * render.pixel_pitch_mm, (y as f64 + 0.5) * render.pixel_pitch_mm);
            let mut expected = 0.0;
            let mut uncertain = false;
            for paint in paints.iter().filter(|a| a.page + 1 == p.page as usize) {
                let near = match paint.boundary.near_boundary(point, uncertainty_mm, &mut budget) {
                    Ok(near) => near,
                    Err(reason) => return Ok(interrupted(results, Some(result), uncertainty_mm, reason)),
                };
                if near {
                    uncertain = true;
                }
                let contains = match paint.boundary.contains_bounded(point, FillRule::NonZero, &mut budget) {
                    Ok(contains) => contains,
                    Err(reason) => return Ok(interrupted(results, Some(result), uncertainty_mm, reason)),
                };
                if contains {
                    // ISO 32000-1 §8.6.7 and Table 148: OP preserves unspecified
                    // colourants. Only direct DeviceCMYK with OPM 1 preserves zero
                    // components; a zero Separation tint still paints its named ink.
                    if let Some(tone) = crate::tone::overpaint(&paint.inks, &p.name, paint.overprint, paint.skip_zero_process) {
                        expected = tone;
                    }
                }
            }
            if uncertain {
                result.boundary_pixels += 1;
                continue;
            }
            result.compared_pixels += 1;
            let observed = 1.0 - f64::from(pixel.0[0]) / 255.0;
            let error = (observed - expected).abs();
            result.maximum_tone_error = result.maximum_tone_error.max(error);
            // Eight-bit quantisation is an engineering bound, not a production tint tolerance.
            if error > 1.0 / 255.0 + f64::from(f32::EPSILON) {
                result.status = Status::Fail;
                result.wrong_pixels += 1;
                result.first_defect_mm.get_or_insert([point.x, point.y]);
            }
        }
        if result.compared_pixels == 0 {
            result.status = Status::Incomplete;
        }
        results.push(result);
    }
    let status = if results.iter().any(|p| p.status == Status::Fail) {
        Status::Fail
    } else if results.iter().any(|p| p.status == Status::Incomplete) {
        Status::Incomplete
    } else {
        Status::Pass
    };
    Ok(Comparison { status, uncertainty_mm, planes: results, issues })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone::tones;
    #[test]
    fn tones_keep_named_ink_tints_and_do_not_invent_rgb_conversion() {
        assert_eq!(tones(&serde_json::json!({"tint":0.25}), Some("Original white")).unwrap()["Original white"], 0.25);
        assert!(tones(&serde_json::json!({"color":{"model":"rgb","r":1,"g":0,"b":0}}), None).is_err());
        assert!(tones(&serde_json::json!({}), Some("Cyan")).is_err());
    }
    // Original hand-authored ISO fixtures, never generated by an external editor.
    fn source_pdf(content: &str, mode: u8, extra: &str) -> Vec<u8> {
        source_pdf_group(content, mode, extra, false, false)
    }
    fn source_pdf_group(content: &str, mode: u8, extra: &str, page_group: bool, form_group: bool) -> Vec<u8> {
        let page_group = if page_group { "/Group << /S /Transparency /CS /DeviceRGB >>" } else { "" };
        let form_group = if form_group { "/Group << /S /Transparency /CS /DeviceRGB >>" } else { "" };
        let stream = format!("1 1 1 1 k 0 0 50 50 re f /GS gs {content}");
        let function = "{ 0 0 }";
        let form = "/Spot cs 0 scn 10 10 10 10 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /ColorSpace << /Spot [/Separation /Cyan /DeviceCMYK 5 0 R] /Duo [/DeviceN [/Cyan /Magenta] /DeviceCMYK 6 0 R] /Alias /DeviceCMYK {extra} >> /ExtGState << /GS 7 0 R >> /XObject << /Form 8 0 R >> >> /Contents 4 0 R {page_group} >>"
            ),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 0 0 0] /N 1 >>".into(),
            format!("<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length {} >>\nstream\n{function}\nendstream", function.len()),
            format!("<< /Type /ExtGState /OP true /op true /OPM {mode} >>"),
            format!("<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] {form_group} /Length {} >>\nstream\n{form}\nendstream", form.len()),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![0];
        for (i, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
        for offset in offsets.iter().skip(1) {
            bytes.extend(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len()).as_bytes());
        bytes
    }
    fn captured(bytes: &[u8]) -> Census {
        let imported = vectorcraft_pdf::import_with_report(bytes, &Default::default()).unwrap();
        assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
        let mut c = crate::pdf::census(&imported.document);
        crate::ordering::reconcile(bytes, &mut c);
        assert!(c.unresolved.is_empty(), "{:?}", c.unresolved);
        assert!(c.paint_order_verified);
        c
    }
    #[test]
    fn original_spaces_survive_strokes_forms_and_graphics_state_restore() {
        use vectorcraft_pdf::PrintColorSpace::*;
        let bytes = source_pdf(
            "q /Spot cs 0 scn 10 10 20 20 re f /Duo CS 0 1 SCN 1 w 10 10 m 30 30 l S Q /Form Do /Alias cs 0 1 0 0 sc 40 40 5 5 re f",
            1,
            "",
        );
        let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
        assert!(raw.issues.is_empty(), "{:?}", raw.issues);
        let kinds: Vec<_> = raw.source_indices[0].iter().map(|i| raw.sources[*i].color_space).collect();
        assert_eq!(kinds, [DeviceCmyk, Separation, DeviceN, Separation, DeviceCmyk]);
        assert_eq!(raw.sources[raw.source_indices[0][1]].colorants, ["Cyan"]);
        assert_eq!(raw.sources[raw.source_indices[0][2]].colorants, ["Cyan", "Magenta"]);
        // Original components and overprint/OPM states have distinct raw identities.
        assert_eq!(raw.sources.len(), 4);
        assert_eq!(
            raw.source_indices[0].iter().map(|i| (raw.sources[*i].overprint, raw.sources[*i].overprint_mode)).collect::<Vec<_>>(),
            [(false, 0), (true, 1), (true, 1), (true, 1), (true, 1)]
        );
        let c = captured(&bytes);
        assert_eq!(c.paints.iter().map(|p| c.paint_sources[p.source_space.unwrap()].color_space).collect::<Vec<_>>(), kinds);
    }
    #[test]
    fn named_process_paints_do_not_write_unnamed_components_or_suppress_zero_tints() {
        for (content, expected) in [
            ("/Spot cs 0 scn 10 10 20 20 re f", BTreeMap::from([("Cyan".into(), 0.0)])),
            ("/Duo cs 0 0.5 scn 10 10 20 20 re f", BTreeMap::from([("Cyan".into(), 0.0), ("Magenta".into(), 0.5)])),
        ] {
            let c = captured(&source_pdf(content, 1, ""));
            let p = c.paints.last().unwrap();
            assert_eq!(p.paint["color"]["model"], "cmyk");
            let source = &c.paint_sources[p.source_space.unwrap()];
            assert_eq!(source_tones(p, source).unwrap(), expected);
            assert_ne!(source.color_space, vectorcraft_pdf::PrintColorSpace::DeviceCmyk);
        }
    }
    #[test]
    fn named_colour_operand_count_cannot_be_repaired_into_a_proof() {
        for content in [
            "/Duo cs 1 scn 10 10 20 20 re f",
            "/Duo cs 1 0 1 scn 10 10 20 20 re f",
            "/Spot cs 1 0 scn 10 10 20 20 re f",
            "/Alias cs 1 0 sc 10 10 20 20 re f",
        ] {
            let raw = vectorcraft_pdf::print_order(&source_pdf(content, 1, "")).unwrap();
            assert!(raw.issues.iter().any(|s| s.contains("Colour component count")), "{content}: {:?}", raw.issues);
        }
    }
    #[test]
    fn source_tones_require_original_components_as_well_as_space_names() {
        let c = captured(&source_pdf("/Duo cs 0.25 0.5 scn 10 10 20 20 re f", 1, ""));
        let paint = c.paints.last().unwrap();
        let source = &c.paint_sources[paint.source_space.unwrap()];
        assert_eq!(source.components, Some(vec![0.25, 0.5]));
        assert!(source_tones(paint, source).is_ok());
        for components in [None, Some(vec![0.25]), Some(vec![0.5, 0.25]), Some(vec![0.0, 0.5]), Some(vec![f64::NAN, 0.5])] {
            let mut changed = source.clone();
            changed.components = components;
            assert!(source_tones(paint, &changed).is_err());
        }
        let mut changed = paint.clone();
        changed.paint["color"]["c"] = serde_json::json!(0.5);
        assert!(source_tones(&changed, source).is_err());
    }
    #[test]
    fn decoded_tones_match_raw_defaults_resets_forms_and_content_windows() {
        let pad = "% bounded content-window control\n".repeat(3000);
        let stream = format!(
            "/Duo cs .25 .5 scn 10 10 2 2 re f q /Spot CS .75 SCN 1 w 10 10 m 30 30 l S Q {pad} /Duo cs 10 10 2 2 re f /Form Do /Duo CS .25 .5 SCN 10 10 m 30 30 l S /Duo CS 10 10 m 30 30 l S"
        );
        let c = captured(&source_pdf(&stream, 1, ""));
        assert_eq!(c.paints.len(), 7);
        for p in &c.paints {
            let source = &c.paint_sources[p.source_space.unwrap()];
            assert!(source_tones(p, source).is_ok(), "{source:?}: {:?}", p.paint);
        }
        let values: Vec<_> = c.paints.iter().map(|p| c.paint_sources[p.source_space.unwrap()].components.clone().unwrap()).collect();
        assert_eq!(values, [vec![1.0; 4], vec![0.25, 0.5], vec![0.75], vec![1.0, 1.0], vec![0.0], vec![0.25, 0.5], vec![1.0, 1.0]]);
    }

    #[test]
    fn nested_forms_restore_both_colour_spaces_and_components_before_painting() {
        let page = "/D cs .2 .4 scn /D CS .6 .8 SCN 1 w q /S cs .5 scn /S CS .25 SCN /Outer Do Q 10 10 2 2 re B";
        let outer = ".75 scn 10 10 2 2 re B /Inner Do 10 10 2 2 re B";
        let inner = "/D cs .1 .9 scn /D CS .3 .7 SCN 10 10 2 2 re B";
        let stream = |s: &str| format!("<< /Length {} >>\nstream\n{s}\nendstream", s.len());
        let form = |s: &str| format!("<< /Type /XObject /Subtype /Form /BBox [0 0 72 72] /Length {} >>\nstream\n{s}\nendstream", s.len());
        let bytes = crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Resources << /ColorSpace << /S [/Separation /Cyan /DeviceCMYK 5 0 R] /D [/DeviceN [/Cyan /Magenta] /DeviceCMYK 6 0 R] >> /XObject << /Outer 7 0 R /Inner 8 0 R >> >> /Contents 4 0 R >>".into(),
            stream(page),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 0 0 0] /N 1 >>".into(),
            "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length 7 >>\nstream\n{ 0 0 }\nendstream".into(),
            form(outer), form(inner),
        ]);
        let c = captured(&bytes);
        let values: Vec<_> = c
            .paints
            .iter()
            .map(|p| {
                let source = &c.paint_sources[p.source_space.unwrap()];
                assert!(source_tones(p, source).is_ok(), "{source:?}: {:?}", p.paint);
                source.components.clone().unwrap()
            })
            .collect();
        assert_eq!(values, [vec![0.75], vec![0.25], vec![0.1, 0.9], vec![0.3, 0.7], vec![0.75], vec![0.25], vec![0.2, 0.4], vec![0.6, 0.8]]);
    }

    #[test]
    fn malformed_named_operands_keep_every_scoped_obligation_unresolved() {
        for (extra, expected) in [("0.5 scn 10 10 2 2 re f", Status::Pass), ("0.5 0.5 scn 10 10 2 2 re f", Status::Incomplete)] {
            let bytes = crate::pdf::tests::fixture_mode(true, 1, "Exact#20ink", extra, None);
            let mut request = crate::tests::request();
            request.identity.pdf_sha256 = crate::sha256(&bytes);
            request.identity.recipe_sha256 = crate::digest(&request.recipe).unwrap();
            let report = crate::pdf::check(&request, &bytes).unwrap();
            assert_eq!(report.coverage.total, crate::enumerate(&request).unwrap().len());
            for facet in ["identity", "layer", "overprint"] {
                let key = format!("file/output/{facet}");
                assert_eq!(report.findings.iter().find(|f| f.obligation.key == key).unwrap().status, expected, "{extra}: {key}");
            }
            assert_ne!(report.status, Status::Pass);
        }
    }

    #[test]
    fn default_space_substitution_is_not_assumed_to_be_direct_device_cmyk() {
        let raw = vectorcraft_pdf::print_order(&source_pdf("/Alias cs 0 0 0 0 sc 10 10 20 20 re f", 1, "/DefaultCMYK [/ICCBased 9 0 R]")).unwrap();
        assert!(raw.sources.iter().all(|s| s.color_space == vectorcraft_pdf::PrintColorSpace::Other));
    }
    #[test]
    fn transparency_group_context_blocks_device_overprint_assumptions() {
        for (page, form) in [(true, false), (false, true)] {
            let bytes = source_pdf_group("/Form Do", 1, "", page, form);
            let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
            assert!(raw.issues.iter().any(|s| s.contains("transparency group colour space")));
            let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
            crate::ordering::reconcile(&bytes, &mut census);
            assert!(!census.paint_order_verified);
            assert!(census.paint_sources.is_empty());
        }
    }
    #[test]
    fn independent_named_process_zero_tints_match_real_planes_in_both_overprint_modes() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else { return };
        for dpi in [72, 144] {
            for mode in [0, 1] {
                for (label, content) in [("separation", "/Spot cs 0 scn 10 10 20 20 re f"), ("devicen", "/Duo cs 0 0.5 scn 10 10 20 20 re f")] {
                    let bytes = source_pdf(content, mode, "");
                    let c = captured(&bytes);
                    let output = std::env::temp_dir().join(format!(
                        "vc-source-space-{label}-{mode}-{dpi}-{}",
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
                    ));
                    let config = crate::independent::Configuration {
                        binary: binary.clone().into(),
                        binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                        version: "10.07.1".into(),
                        dpi,
                        timeout_ms: 10_000,
                        max_output_bytes: 16 * 1024 * 1024,
                        qualification_sha256: None,
                    };
                    let render = crate::independent::render(&config, &bytes, &output).unwrap();
                    let result = compare(&c, &render, Some(0.6)).unwrap();
                    assert_eq!(result.status, Status::Pass, "{label}/{mode}/{dpi}: {result:?}");
                    let mut wrong = c.clone();
                    let index = wrong.paints.last().unwrap().source_space.unwrap();
                    wrong.paint_sources[index].color_space = vectorcraft_pdf::PrintColorSpace::DeviceCmyk;
                    // Inconsistent source metadata is now refused before pixel comparison.
                    assert_eq!(compare(&wrong, &render, Some(0.6)).unwrap().status, Status::Incomplete);
                    // Keep the older adversarial model check too: a coherently forged
                    // direct-CMYK source must disagree with the real named-ink planes.
                    let colour = &wrong.paints.last().unwrap().paint["color"];
                    wrong.paint_sources[index].components = Some(["c", "m", "y", "k"].iter().map(|k| colour[k].as_f64().unwrap()).collect());
                    assert_eq!(compare(&wrong, &render, Some(0.6)).unwrap().status, Status::Fail, "wrong model {label}/{mode}/{dpi}");
                    wrong.paint_order_verified = false;
                    assert_eq!(compare(&wrong, &render, Some(0.6)).unwrap().status, Status::Incomplete);
                    assert!(!render.qualified);
                    std::fs::remove_dir_all(output).unwrap();
                }
            }
        }
    }
}
