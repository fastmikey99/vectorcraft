//! Bounded source continuity for a single preserved image. A matching outline is
//! insufficient: normalized samples, alpha, pixel grid, page and route must agree.
use crate::{Evidence, Report, Request, Status, attribution::Plan, prepared_mask::Mask};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use vectorcraft_geom::{Affine, FillRule, PathData, Point};

fn same_coverage(a: &PathData, b: &PathData) -> Result<bool, String> {
    if a == b {
        return Ok(true);
    }
    vectorcraft_pathops::try_boolean_for_measurement(a, FillRule::NonZero, b, FillRule::NonZero, vectorcraft_pathops::BoolOp::Xor)
        .map(|p| p.is_empty())
        .map_err(|e| e.to_string())
}
pub(crate) fn process_model(paint: &Value) -> Option<&str> {
    match paint.pointer("/description/colour_space").and_then(Value::as_str)? {
        "DeviceGray" => Some("DeviceGray"),
        "DeviceCMYK" => Some("DeviceCMYK"),
        "Indexed" => paint.get("indexed_process_base").and_then(Value::as_str).filter(|model| matches!(*model, "DeviceGray" | "DeviceCMYK")),
        _ => None,
    }
}
fn supported(paint: &Value) -> bool {
    process_model(paint).is_some() && paint.get("sample_sha256").and_then(Value::as_str).is_some_and(|s| s.len() == 64)
}
struct Grid {
    width: u32,
    height: u32,
    matrix: [f64; 6],
}
fn visible_grid(mask: &Mask, window: [u32; 4]) -> Grid {
    let [x, y, width, height] = window;
    Grid { width, height, matrix: (Affine::new(mask.pixel_to_page_mm) * Affine::translate((f64::from(x), f64::from(y)))).as_coeffs() }
}
fn displacement(a: &Grid, b: [f64; 6]) -> (f64, f64) {
    let xa = Affine::new(a.matrix);
    let xb = Affine::new(b);
    let mut distance: f64 = 0.0;
    let mut magnitude: f64 = 1.0;
    for x in [0.0, f64::from(a.width)] {
        for y in [0.0, f64::from(a.height)] {
            let pa = xa * Point::new(x, y);
            let pb = xb * Point::new(x, y);
            distance = distance.max(pa.distance(pb));
            magnitude = magnitude.max(pa.x.abs()).max(pa.y.abs()).max(pb.x.abs()).max(pb.y.abs());
        }
    }
    // Bounded arithmetic envelope, separate from approved manufacturing tolerance.
    (distance, 128.0 * f64::EPSILON * magnitude + 1.0e-9)
}
/// Hash a lossless pixel-grid orientation in the existing normalized sample domain.
/// At most eight candidates over the already bounded one-megapixel capture. No
/// resampling or free alignment is allowed; cell centres map to exact source cells.
fn oriented_hash(samples: &vectorcraft_pdf::PrintImageSamples, mask: &Mask, width: u32, height: u32, grid: Affine) -> Result<String, String> {
    let pixels = (width as usize).checked_mul(height as usize).ok_or("Raster orientation dimensions overflow")?;
    if pixels == 0 || pixels > 1_048_576 || samples.colourants.len() > 4 {
        return Err("Raster orientation work budget exceeded".into());
    }
    let index = |pixel: usize| -> Result<usize, String> {
        let x = pixel % width as usize;
        let y = pixel / width as usize;
        let point = grid * Point::new(x as f64 + 0.5, y as f64 + 0.5);
        if !point.x.is_finite()
            || !point.y.is_finite()
            || point.x < 0.0
            || point.y < 0.0
            || point.x >= f64::from(samples.width)
            || point.y >= f64::from(samples.height)
        {
            return Err("Raster orientation leaves the observed sample grid".into());
        }
        Ok(point.y.floor() as usize * samples.width as usize + point.x.floor() as usize)
    };
    let alpha = |at: usize| -> Result<f64, String> {
        if mask.cell_visible((at % samples.width as usize) as u32, (at / samples.width as usize) as u32) { samples.alpha(at) } else { Ok(0.0) }
    };
    let mut hash = Sha256::new();
    hash.update(b"vectorcraft-visible-image-samples-v1\0");
    hash.update(width.to_be_bytes());
    hash.update(height.to_be_bytes());
    for name in &samples.colourants {
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
    }
    for pixel in 0..pixels {
        for channel in 0..samples.colourants.len() {
            let at = index(pixel)?;
            let tone = if alpha(at)? == 0.0 { 0.0 } else { samples.tone(at, channel)? };
            hash.update(tone.to_be_bytes());
        }
    }
    for pixel in 0..pixels {
        hash.update(alpha(index(pixel)?)?.to_be_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}
/// Source-bound visible sample proof. Hidden colours and transparent padding
/// cannot change observable printed conformity. The raw hash is retained separately.
pub(crate) fn visible_signature(mask: &Mask, samples: &vectorcraft_pdf::PrintImageSamples) -> Result<Value, String> {
    if mask.width != samples.width || mask.height != samples.height {
        return Err("Visible sample dimensions differ from mask".into());
    }
    let Some(window) = mask.visible_window()? else {
        return Ok(Value::Null);
    };
    let [x, y, w, h] = window;
    let hash = oriented_hash(samples, mask, w, h, Affine::translate((f64::from(x), f64::from(y))))?;
    Ok(json!({"window":window,"sha256":hash}))
}
fn orientations(expected: &Grid, actual: &Grid) -> Vec<[f64; 6]> {
    let (w, h) = (f64::from(actual.width), f64::from(actual.height));
    let mut grids = vec![];
    if expected.width == actual.width && expected.height == actual.height {
        grids.extend([[1.0, 0.0, 0.0, 1.0, 0.0, 0.0], [-1.0, 0.0, 0.0, 1.0, w, 0.0], [1.0, 0.0, 0.0, -1.0, 0.0, h], [-1.0, 0.0, 0.0, -1.0, w, h]]);
    }
    if expected.width == actual.height && expected.height == actual.width {
        grids.extend([[0.0, 1.0, 1.0, 0.0, 0.0, 0.0], [0.0, 1.0, -1.0, 0.0, w, 0.0], [0.0, -1.0, 1.0, 0.0, 0.0, h], [0.0, -1.0, -1.0, 0.0, w, h]]);
    }
    grids
}
fn evaluate(request: &Request, plan: &Plan, bytes: &[u8]) -> Result<(Status, Value), String> {
    let inputs = request.geometry.as_ref().ok_or("Prepared raster geometry missing")?;
    let baseline = request.baseline.as_ref().ok_or("Prepared baseline missing")?;
    if inputs.regions.len() != 1 {
        return Err("Multiple or mixed raster sources require unique sample and compositing reconciliation".into());
    }
    let (id, geometry) = inputs.regions.first_key_value().ok_or("Prepared raster region missing")?;
    let mask = geometry.prepared_mask.as_ref().ok_or("Explicit prepared source mask missing")?;
    if !supported(&geometry.paint) {
        return Err("Raster source colour-space definition is not qualified for sample continuity".into());
    }
    let (placed, replayed_path, placement_error) = crate::prepare_edits::raster_placement(mask, id, &baseline.intentional_edits)?;
    if placed.page != geometry.page || replayed_path != geometry.path_mm {
        return Err("Prepared source grid does not reproduce the approved geometry".into());
    }
    let program = crate::program::execute(request, plan)?;
    if !program.unresolved.is_empty() || program.outputs.len() != 1 {
        return Err("Raster output graph is unresolved or requires multiple-output accounting".into());
    }
    let (key, pieces) = program.outputs.first_key_value().ok_or("Raster output missing")?;
    let links: Vec<_> = request.recipe.file_links.iter().filter(|l| request.garment.matches(&l.garment)).collect();
    let [link] = links.as_slice() else {
        return Err("Raster source continuity requires a single unambiguous output route".into());
    };
    if link.key != *key || link.name_mode != "source" || link.destination != "Indigo" || link.overprint {
        return Err("Raster source continuity requires an explicit preserved composite knockout route".into());
    }
    let [piece] = pieces.as_slice() else {
        return Err("Raster construction requires a single preserved source piece".into());
    };
    let normalized_source = vectorcraft_pathops::try_boolean_for_measurement(
        &geometry.path_mm,
        geometry.fill_rule,
        &PathData::default(),
        FillRule::NonZero,
        vectorcraft_pathops::BoolOp::Union,
    )
    .map_err(|e| e.to_string())?;
    if piece.regions.len() != 1
        || !piece.regions.contains(id)
        || piece.page != placed.page
        || piece.paint != geometry.paint
        || !same_coverage(&piece.path, &normalized_source)?
    {
        return Err("Raster construction changes source paint or boundary; its extension/crop policy needs qualification".into());
    }
    let output = crate::prepare::capture(bytes, "continuity-observation".into(), request.identity.job_revision.clone())?;
    if output.raster_sources.len() != 1 {
        return Err(format!("Output is not one fully accounted binary-alpha image: {}", output.diagnostics.join("; ")));
    };
    let actual = output.raster_sources.values().next().ok_or("Observed raster source missing")?;
    if !supported(&actual.paint) {
        return Err("Output raster colour-space definition is not qualified for sample continuity".into());
    }
    let window = mask.visible_window()?.ok_or("Empty raster source requires separate omission/emptiness reconciliation")?;
    if geometry.paint.pointer("/visible_sample/window") != Some(&json!(window)) {
        return Err("Visible raster source proof is missing or stale; recapture from the original".into());
    }
    let expected_hash = geometry.paint.pointer("/visible_sample/sha256").and_then(Value::as_str).ok_or("Visible raster source hash missing")?;
    let Some(actual_window) = actual.mask.visible_window()? else {
        return Ok((
            Status::Fail,
            json!({"scope":"single_preserved_binary_alpha_image","source_region":id,"file_link":key,
            "defect":"Expected visible source replaced by a fully transparent image","expected_visible_window":window}),
        ));
    };
    let expected_grid = visible_grid(&placed, window);
    let actual_grid = visible_grid(&actual.mask, actual_window);
    let grids = orientations(&expected_grid, &actual_grid);
    if grids.is_empty() {
        return Err("Changed raster dimensions require resampling or tiling equivalence qualification".into());
    }
    let order = vectorcraft_pdf::print_order(bytes).map_err(|e| e.to_string())?;
    let images: Vec<_> = order.images.iter().flatten().collect();
    let [image] = images.as_slice() else {
        return Err("Raw raster output census is ambiguous".into());
    };
    let layer = image.layer.and_then(|i| order.layers.get(i));
    if order.layers.iter().filter(|l| crate::routes::layer_matches(&link.pdf_layer, l)).count() > 1 {
        return Err("Duplicate output layer identities prevent raster route reconciliation".into());
    }
    let tolerance = request
        .acceptance
        .get("geometry_tolerance_mm")
        .filter(|f| f.evidenced() && crate::digest(&request.acceptance).ok().as_ref() == Some(&request.identity.measurement_sha256))
        .and_then(|f| f.value.as_f64())
        .filter(|n| n.is_finite() && *n >= 0.0);
    let samples = image.samples.as_ref().ok_or("Observed normalized raster samples missing")?;
    let model_agrees = geometry.paint["colourants"] == actual.paint["colourants"] && process_model(&geometry.paint) == process_model(&actual.paint);
    let mut matched = vec![];
    if model_agrees {
        for grid in grids {
            let grid = Affine::new(grid);
            let samples_grid = Affine::translate((f64::from(actual_window[0]), f64::from(actual_window[1]))) * grid;
            let hash = oriented_hash(samples, &actual.mask, expected_grid.width, expected_grid.height, samples_grid)?;
            if hash == expected_hash {
                let oriented = (Affine::new(actual_grid.matrix) * grid).as_coeffs();
                let (distance, error) = displacement(&expected_grid, oriented);
                matched.push((distance, error + placement_error, grid.as_coeffs()));
            }
        }
    }
    matched.sort_by(|a, b| a.0.total_cmp(&b.0));
    let samples_agree = !matched.is_empty();
    let best = matched.first().copied();
    let page_agrees = placed.page == actual.mask.page;
    let layer_agrees = layer.is_some_and(|actual| crate::routes::layer_matches(&link.pdf_layer, actual));
    let boundary = if mask.clip.is_some() || actual.mask.clip.is_some() {
        Some(crate::prepared_crop::compare(&placed, &actual.mask, tolerance, placement_error))
    } else {
        None
    };
    let status = if boundary.as_ref().is_some_and(|b| b.as_ref().is_ok_and(|m| m.status == Status::Fail))
        || !samples_agree
        || !page_agrees
        || !layer_agrees
        || tolerance.is_some_and(|t| matched.iter().all(|(d, e, _)| (d - e).max(0.0) > t))
    {
        Status::Fail
    } else if boundary.as_ref().is_none_or(|b| b.as_ref().is_ok_and(|m| m.status == Status::Pass))
        && tolerance.is_some_and(|t| matched.iter().any(|(d, e, _)| d + e <= t))
    {
        Status::Pass
    } else {
        Status::Incomplete
    };
    let mut observed = json!({"scope":"single_preserved_binary_alpha_image","source_region":id,"file_link":key,
        "samples_agree":samples_agree,"page_agrees":page_agrees,"layer_agrees":layer_agrees,
        "expected_visible_sample_sha256":expected_hash,"expected_visible_window":window,"observed_visible_window":actual_window,
        "expected_sample_sha256":geometry.paint["sample_sha256"],"observed_sample_sha256":actual.paint["sample_sha256"],
        "expected_pixel_to_page_mm":placed.pixel_to_page_mm,"observed_pixel_to_page_mm":actual.mask.pixel_to_page_mm,
        "max_corner_displacement_mm":best.map(|v|v.0),"numerical_error_mm":best.map(|v|v.1),"sample_grid_orientation":best.map(|v|v.2),"matching_orientations":matched.len(),"acceptance_mm":tolerance,
        "source_original_replay":"separate_required_obligation"});
    if let Some(boundary) = boundary {
        observed["retained_crop_boundary"] = match boundary {
            Ok(measurement) => json!(measurement),
            Err(reason) => json!({"status":"incomplete","reason":reason}),
        };
    }
    Ok((status, observed))
}
pub fn check(request: &Request, plan: &Plan, bytes: &[u8], report: &mut Report) -> Result<(), String> {
    if request.geometry.as_ref().is_none_or(|i| !i.regions.values().any(|g| g.paint.get("type").and_then(Value::as_str) == Some("source_image"))) {
        return Ok(());
    }
    if report.findings.iter().any(|f| f.obligation.key == "global/raster" && f.status == Status::Fail) {
        return Ok(());
    }
    let (status, reason, observed) = match evaluate(request, plan, bytes) {
        Ok((status, observed)) => (status, "Preserved raster samples, binary alpha, approved pixel grid and exact output route compared; original replay and independent rendering remain separate".into(), observed),
        Err(reason) => (Status::Incomplete, reason, Value::Null),
    };
    report.record(
        "global/raster",
        status,
        reason,
        observed,
        vec![
            Evidence {
                artifact_sha256: request.identity.baseline_sha256.clone(),
                producer: "checks/approved-raster-source-1".into(),
                detail: "Prepared source sample identity and explicitly replayed pixel grid".into(),
            },
            Evidence {
                artifact_sha256: crate::sha256(bytes),
                producer: "checks/raster-source-continuity-1".into(),
                detail: "Fresh public PDF sample census compared to the approved source; no alignment inferred from output bounds".into(),
            },
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preparation::{Fact, Provenance};
    fn fact(value: Value) -> Fact {
        Fact {
            value,
            provenance: Provenance::Operator {
                actor: "Synthetic author".into(),
                at: "2026-10-19".into(),
                decision_id: "Synthetic continuity criterion only".into(),
                evidence_sha256: "e".repeat(64),
            },
        }
    }
    fn image(samples: &str, alpha: &str, matrix: &str, layer: &str, extra: &str, model: &str) -> Vec<u8> {
        image_with_mask_intent(samples, alpha, matrix, layer, extra, model, "")
    }
    fn image_with_mask_intent(samples: &str, alpha: &str, matrix: &str, layer: &str, extra: &str, model: &str, mask_intent: &str) -> Vec<u8> {
        image_with_context(samples, alpha, matrix, layer, extra, model, (mask_intent, ""))
    }
    fn image_with_context(samples: &str, alpha: &str, matrix: &str, layer: &str, extra: &str, model: &str, context: (&str, &str)) -> Vec<u8> {
        let (mask_intent, state) = context;
        let space = if model.starts_with('[') { model.to_owned() } else { format!("/{model}") };
        let resources = if state.is_empty() { String::new() } else { format!(" /ExtGState << {state} >>") };
        let content = format!("/OC /Layer BDC q {matrix} cm /Im Do Q {extra} EMC");
        crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [7 0 R] /D << /Order [7 0 R] /ON [7 0 R] >> >> >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 144 144] /Resources << /Properties << /Layer 7 0 R >> /XObject << /Im 5 0 R >>{resources} >> /Contents 4 0 R >>"
            ),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            format!(
                "<< /Type /XObject /Subtype /Image /Width 3 /Height 3 /BitsPerComponent 8 /ColorSpace {space} /SMask 6 0 R /Filter /ASCIIHexDecode /Length {} >>\nstream\n{samples}>\nendstream",
                samples.len() + 1
            ),
            format!(
                "<< /Type /XObject /Subtype /Image /Width 3 /Height 3 /BitsPerComponent 8 /ColorSpace /DeviceGray {mask_intent} /Filter /ASCIIHexDecode /Length {} >>\nstream\n{alpha}>\nendstream",
                alpha.len() + 1
            ),
            format!("<< /Type /OCG /Name ({layer}) >>"),
        ])
    }
    const SAMPLES: &str = "0020406080a0c0e0ff";
    const ALPHA: &str = "ffffffff00ffffffff";
    const MATRIX: &str = "36 0 0 36 18 18";
    pub(super) fn original() -> Vec<u8> {
        image(SAMPLES, ALPHA, MATRIX, "Source", "", "DeviceGray")
    }
    fn request(bytes: &[u8], edits: &[Fact]) -> Request {
        request_for_original(&original(), bytes, edits)
    }
    pub(super) fn request_for_original(original: &[u8], bytes: &[u8], edits: &[Fact]) -> Request {
        let mut r = crate::attribution::tests::request();
        let draft = crate::prepare::capture(original, "synthetic".into(), r.identity.job_revision.clone()).unwrap();
        let (id, source) = draft.raster_sources.first_key_value().unwrap();
        let mut decisions =
            vec![fact(json!({"kind":"raster_mask","source":id,"source_sha256":source.mask.source_sha256,"policy":source.mask.boundary_policy()}))];
        decisions.extend_from_slice(edits);
        let mut prepared = crate::prepare_edits::replay(draft, &decisions).unwrap();
        prepared.baseline.approval = Some(fact(json!("approved")));
        r.baseline = Some(prepared.baseline);
        r.geometry = Some(prepared.geometry);
        r.recipe.rules[0].operation = "preserve".into();
        r.recipe.rules[0].distance_mm = None;
        r.recipe.rules[0].join = None;
        r.recipe.required_screens = Some(vec![]);
        let link = &mut r.recipe.file_links[0];
        link.pdf_layer = "TECH Output".into();
        link.name_mode = "source".into();
        link.separation_name = None;
        link.destination = "Indigo".into();
        link.overprint = false;
        let f = r.recipe.foundation.as_mut().unwrap();
        f["treatments"]["arbitrary"]["paint"] = json!({"mode":"source"});
        f["input_routes"][0]["destination"] = json!("Indigo");
        for plate in f["verification"]["plates"].as_array_mut().unwrap() {
            plate["name"] = Value::Null;
            plate["name_mode"] = json!("composite");
            plate["destination"] = json!("Indigo");
            plate["required"] = json!(false);
        }
        assert!(crate::contract::issues(&r.recipe).is_empty(), "{:?}", crate::contract::issues(&r.recipe));
        r.acceptance.insert("geometry_tolerance_mm".into(), fact(json!(0.01)));
        r.identity.original_sha256 = crate::sha256(original);
        r.identity.pdf_sha256 = crate::sha256(bytes);
        refresh(&mut r);
        r
    }
    fn refresh(r: &mut Request) {
        r.identity.baseline_sha256 = crate::digest(r.baseline.as_ref().unwrap()).unwrap();
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        r.identity.measurement_sha256 = crate::digest(&r.acceptance).unwrap();
        r.identity.assignments_sha256 = r.assignments_digest().unwrap();
    }
    fn check(name: &str, r: &Request, bytes: &[u8], expected: Status) {
        check_source(name, r, bytes, &original(), expected);
    }
    pub(super) fn check_source(name: &str, r: &Request, bytes: &[u8], original: &[u8], expected: Status) {
        assert!(crate::prepare::verify_original(r, original).unwrap().is_empty());
        let report = crate::pdf::check(r, bytes).unwrap();
        let finding = report.findings.iter().find(|f| f.obligation.key == "global/raster").unwrap();
        assert_eq!(finding.status, expected, "{name}: {}", serde_json::to_string_pretty(&report).unwrap());
        if let Ok(dir) = std::env::var("CHECK_RASTER_CONTINUITY_EVIDENCE_DIR") {
            let dir = std::path::Path::new(&dir);
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(format!("{name}.pdf")), bytes).unwrap();
            std::fs::write(dir.join(format!("{name}.json")), serde_json::to_vec_pretty(r).unwrap()).unwrap();
            std::fs::write(dir.join(format!("{name}-expected.json")), serde_json::to_vec(&json!({"global/raster":expected})).unwrap()).unwrap();
            std::fs::write(dir.join(format!("{name}-original.pdf")), original).unwrap();
        }
    }
    #[test]
    fn indexed_process_samples_preserve_visible_content_across_lossless_encodings() {
        for (label, base, colours) in [("gray", "DeviceGray", ["00", "40", "ff"]), ("cmyk", "DeviceCMYK", ["ff000000", "00ff0000", "0000ff00"])] {
            let palette = colours.concat();
            let indexed = format!("[/Indexed /{base} 2 <{palette}>]");
            let indices = "000102000102000102";
            let original = image(indices, ALPHA, MATRIX, "Source", "", &indexed);
            let reordered = format!("[/Indexed /{base} 2 <{}{}{}>]", colours[1], colours[2], colours[0]);
            let direct = colours.repeat(3).concat();
            let extra = if base == "DeviceGray" { "7f" } else { "7f7f7f7f" };
            let expanded = format!("[/Indexed /{base} 3 <{palette}{extra}>]");
            let changed = format!("[/Indexed /{base} 2 <{extra}{}{}>]", colours[1], colours[2]);
            for (case, samples, alpha, matrix, layer, space, expected) in [
                ("identical", indices, ALPHA, MATRIX, "TECH Output", indexed.as_str(), Status::Pass),
                ("reordered-palette", "020001020001020001", ALPHA, MATRIX, "TECH Output", reordered.as_str(), Status::Pass),
                ("direct-equivalent", direct.as_str(), ALPHA, MATRIX, "TECH Output", base, Status::Pass),
                ("unused-entry", indices, ALPHA, MATRIX, "TECH Output", expanded.as_str(), Status::Pass),
                ("hidden-index", "000102000302000102", ALPHA, MATRIX, "TECH Output", expanded.as_str(), Status::Pass),
                ("visible-palette", indices, ALPHA, MATRIX, "TECH Output", changed.as_str(), Status::Fail),
                ("visible-index", "010102000102000102", ALPHA, MATRIX, "TECH Output", indexed.as_str(), Status::Fail),
                ("changed-alpha", indices, "ffffffffffffffff00", MATRIX, "TECH Output", indexed.as_str(), Status::Fail),
                ("changed-layer", indices, ALPHA, MATRIX, "Wrong output", indexed.as_str(), Status::Fail),
                ("translated", indices, ALPHA, "36 0 0 36 19 18", "TECH Output", indexed.as_str(), Status::Fail),
                ("fractional-alpha", indices, "ffffffff80ffffffff", MATRIX, "TECH Output", indexed.as_str(), Status::Incomplete),
            ] {
                let bytes = image(samples, alpha, matrix, layer, "", space);
                let request = request_for_original(&original, &bytes, &[]);
                check_source(&format!("indexed-{label}-{case}"), &request, &bytes, &original, expected);
            }
            let direct_original = image(&direct, ALPHA, MATRIX, "Source", "", base);
            let indexed_output = image(indices, ALPHA, MATRIX, "TECH Output", "", &indexed);
            let request = request_for_original(&direct_original, &indexed_output, &[]);
            check_source(&format!("indexed-{label}-from-direct"), &request, &indexed_output, &direct_original, Status::Pass);
        }
    }
    #[test]
    fn exported_cmyk_image_masks_preserve_continuity_and_detect_source_changes() {
        let indexed = "[/Indexed /DeviceCMYK 2 <ff00000000ff00000000ff00>]";
        let indices = "000102000102000102";
        let source = image(indices, ALPHA, MATRIX, "Source", "", indexed);
        let options = vectorcraft_pdf::PdfOptions {
            settings: vectorcraft_pdf::PdfSettings { disable_image_interpolation: true, create_layers: true, ..Default::default() },
            ..Default::default()
        };
        for (case, samples, alpha, expected) in [
            ("unchanged", indices, ALPHA, Status::Pass),
            ("changed-ink", "010102000102000102", ALPHA, Status::Fail),
            ("changed-alpha", indices, "ffffffffffffffff00", Status::Fail),
            ("fractional-alpha", indices, "ffffffff80ffffffff", Status::Incomplete),
        ] {
            let input = image(samples, alpha, MATRIX, "TECH Output", "", indexed);
            let mut doc = vectorcraft_pdf::import(&input).unwrap();
            for round in 1..=2 {
                let bytes = vectorcraft_pdf::export(&doc, &options).unwrap();
                let request = request_for_original(&source, &bytes, &[]);
                check_source(&format!("exported-cmyk-{case}-{round}"), &request, &bytes, &source, expected);
                let report = crate::pdf::check(&request, &bytes).unwrap();
                assert_eq!(report.validity, crate::Validity::Provisional);
                assert_ne!(report.status, Status::Pass);
                assert_eq!(report.findings.iter().find(|f| f.obligation.key == "global/independent").unwrap().status, Status::Incomplete);
                doc = vectorcraft_pdf::import(&bytes).unwrap();
            }
        }
        let input = image(indices, ALPHA, MATRIX, "TECH Output", "", indexed);
        let mut doc = vectorcraft_pdf::import(&input).unwrap();
        for case in ["interpolation", "parent-opacity"] {
            let mut options = options.clone();
            if case == "interpolation" {
                options.settings.disable_image_interpolation = false;
            } else {
                std::sync::Arc::make_mut(&mut doc.layers[0]).opacity = 0.5;
            }
            let bytes = vectorcraft_pdf::export(&doc, &options).unwrap();
            let request = request_for_original(&source, &bytes, &[]);
            check_source(&format!("exported-cmyk-{case}"), &request, &bytes, &source, Status::Incomplete);
        }
    }
    #[test]
    fn indexed_and_direct_process_encodings_match_independent_planes_and_detect_defects() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else { return };
        let base = std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        for (label, model, colours, channel) in
            [("gray", "DeviceGray", ["00", "40", "ff"], "Black"), ("cmyk", "DeviceCMYK", ["ff000000", "00ff0000", "0000ff00"], "Cyan")]
        {
            let indexed = format!("[/Indexed /{model} 2 <{}>]", colours.concat());
            let reordered = format!("[/Indexed /{model} 2 <{}{}{}>]", colours[1], colours[2], colours[0]);
            let direct = colours.repeat(3).concat();
            for dpi in [72, 144] {
                let mut canonical = None;
                for (variant, samples, space) in [
                    ("indexed", "000102000102000102", indexed.as_str()),
                    ("reordered", "020001020001020001", reordered.as_str()),
                    ("direct", direct.as_str(), model),
                ] {
                    let bytes = image(samples, ALPHA, MATRIX, "TECH Output", "", space);
                    let output = base.join(format!("indexed-continuity-{stamp}-{label}-{variant}-{dpi}"));
                    let config = crate::independent::Configuration {
                        binary: binary.clone().into(),
                        binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                        version: "10.07.1".into(),
                        dpi,
                        timeout_ms: 10000,
                        max_output_bytes: 16 * 1024 * 1024,
                        qualification_sha256: None,
                    };
                    let mut render = crate::independent::render(&config, &bytes, &output).unwrap();
                    let positive = crate::raster_images::compare(&bytes, &render, Some(0.6)).unwrap();
                    assert_eq!(positive.status, Status::Pass, "{label}/{variant}/{dpi}: {positive:?}");
                    let signatures: std::collections::BTreeMap<_, _> = render
                        .planes
                        .iter()
                        .map(|p| {
                            let pixels = image::open(&p.path).unwrap().to_luma8();
                            ((p.page, p.name.clone()), (p.width, p.height, crate::sha256(pixels.as_raw())))
                        })
                        .collect();
                    if let Some(previous) = &canonical {
                        assert_eq!(&signatures, previous);
                    } else {
                        canonical = Some(signatures.clone());
                    }
                    let original_render = serde_json::to_value(&render).unwrap();
                    let plane = render.planes.iter_mut().find(|p| p.name == channel).unwrap();
                    let mut pixels = image::open(&plane.path).unwrap().to_luma8();
                    assert_eq!(pixels.get_pixel(1, 1).0[0], 255);
                    pixels.put_pixel(1, 1, image::Luma([0]));
                    plane.path = output.join(format!("defect-{channel}.tif"));
                    pixels.save(&plane.path).unwrap();
                    plane.sha256 = crate::sha256(&std::fs::read(&plane.path).unwrap());
                    plane.minimum = 0;
                    let negative = crate::raster_images::compare(&bytes, &render, Some(0.6)).unwrap();
                    assert_eq!(negative.status, Status::Fail, "{label}/{variant}/{dpi}: {negative:?}");
                    std::fs::write(output.join("qualification.json"), serde_json::to_vec_pretty(&json!({
                        "engine":crate::ENGINE_VERSION, "pdf_sha256":crate::sha256(&bytes), "dpi":dpi,
                        "positive":positive, "negative":negative, "render":original_render, "defect_render":render, "configuration":config,
                        "unmodified_pixel_signatures":signatures.iter().map(|((page,name),value)| json!({"page":page,"name":name,"value":value})).collect::<Vec<_>>(),
                        "note":"Original planes and intentionally changed defect plane are retained separately."
                    })).unwrap()).unwrap();
                }
            }
        }
    }
    #[test]
    fn indexed_base_identity_is_not_inferred_from_process_colourant_names() {
        for (space, expected) in [
            ("[/Indexed /DeviceGray 2 <0040ff>]", Some("DeviceGray")),
            ("[/Indexed /DeviceCMYK 2 <ff00000000ff00000000ff00>]", Some("DeviceCMYK")),
            ("[/Indexed [/Separation /Black /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>] 2 <0040ff>]", None),
        ] {
            let bytes = image("000102000102000102", ALPHA, MATRIX, "Source", "", space);
            let order = vectorcraft_pdf::print_order(&bytes).unwrap();
            let samples = order.images[0][0].samples.as_ref().unwrap();
            assert_eq!(samples.process_base(), expected);
            assert!(!samples.direct_cmyk, "Indexed base identity must not change OPM zero handling");
            let mut capture = crate::prepare::capture(&bytes, "synthetic".into(), "job".into()).unwrap();
            if let Some((id, source)) = capture.raster_sources.first_key_value() {
                assert_eq!(process_model(&source.paint), expected);
                assert_eq!(source.paint["visible_sample"].is_null(), expected.is_none());
                let (id, hash) = (id.clone(), source.mask.source_sha256.clone());
                crate::raster_prepare::apply(&mut capture, &id, &hash, "exact_binary_alpha").unwrap();
                let paint = capture.baseline.regions[0].facts.get("paint").and_then(|fact| fact.value.as_str());
                assert_eq!(paint, expected.map(|model| if model == "DeviceGray" { "gray" } else { "cmyk" }));
            } else {
                assert!(expected.is_none(), "{:?}", capture.diagnostics);
            }
        }
        let forged = json!({"description":{"colour_space":"Separation"},"indexed_process_base":"DeviceGray"});
        assert!(process_model(&forged).is_none());
        let unknown = json!({"description":{"colour_space":"Indexed"},"colourants":["Black"]});
        assert!(process_model(&unknown).is_none());
    }
    #[test]
    fn explicit_intent_samples_do_not_qualify_preparation_or_continuity() {
        for intent in ["RelativeColorimetric", "AbsoluteColorimetric", "Perceptual", "Saturation"] {
            let bytes = image(SAMPLES, ALPHA, MATRIX, "TECH Output", "", &format!("DeviceGray /Intent /{intent}"));
            let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
            assert!(raw.images[0][0].samples.is_some() && raw.images[0][0].compositing_unresolved);
            let draft = crate::prepare::capture(&bytes, "synthetic".into(), "job".into()).unwrap();
            assert!(draft.raster_sources.is_empty() && !draft.diagnostics.is_empty());
            let r = request(&bytes, &[]);
            check(&format!("explicit-intent-{intent}"), &r, &bytes, Status::Incomplete);
        }
    }
    #[test]
    fn mask_intent_alone_does_not_qualify_preparation_or_continuity() {
        for intent in ["RelativeColorimetric", "AbsoluteColorimetric", "Perceptual", "Saturation"] {
            let bytes = image_with_mask_intent(SAMPLES, ALPHA, MATRIX, "TECH Output", "", "DeviceGray", &format!("/Intent /{intent}"));
            let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
            assert!(raw.images[0][0].samples.is_some() && raw.images[0][0].description.rendering_intent.is_none());
            assert!(raw.images[0][0].description.mask_rendering_intent.is_some() && raw.images[0][0].compositing_unresolved);
            let draft = crate::prepare::capture(&bytes, "synthetic".into(), "job".into()).unwrap();
            assert!(draft.raster_sources.is_empty() && !draft.diagnostics.is_empty());
            let r = request(&bytes, &[]);
            check(&format!("mask-intent-{intent}"), &r, &bytes, Status::Incomplete);
        }
    }
    #[test]
    fn unresolved_graphics_state_cannot_pass_raster_continuity() {
        for (name, dictionary, expected) in [
            ("missing", "", Status::Incomplete),
            ("malformed", "/State 42", Status::Incomplete),
            ("bad-stroke-flag", "/State << /OP 42 >>", Status::Incomplete),
            ("bad-fill-flag", "/State << /op /Wrong >>", Status::Incomplete),
            ("bad-mode", "/State << /OPM 2 >>", Status::Incomplete),
            ("fractional-mode", "/State << /OPM 0.5 >>", Status::Incomplete),
            ("valid-mode-zero", "/State << /OPM 0 >>", Status::Pass),
            ("valid-mode-one", "/State << /OPM 1 >>", Status::Pass),
        ] {
            let bytes = image_with_context(SAMPLES, ALPHA, &format!("/State gs {MATRIX}"), "TECH Output", "", "DeviceGray", ("", dictionary));
            let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
            assert_eq!(raw.images[0][0].compositing_unresolved, expected == Status::Incomplete, "{name}");
            let draft = crate::prepare::capture(&bytes, "synthetic".into(), "job".into()).unwrap();
            if expected == Status::Incomplete {
                assert!(draft.raster_sources.is_empty());
                assert!(draft.diagnostics.iter().any(|d| d.contains("graphics context")), "{name}: {:?}", draft.diagnostics);
            }
            let r = request(&bytes, &[]);
            check(&format!("graphics-state-{name}"), &r, &bytes, expected);
        }
    }
    #[test]
    fn retained_samples_alpha_and_pixel_grid_are_independent_of_outer_bounds() {
        for (name, samples, alpha, matrix, layer, extra, model, status) in [
            ("identical", SAMPLES, ALPHA, MATRIX, "TECH Output", "", "DeviceGray", Status::Pass),
            ("tone", "1120406080a0c0e0ff", ALPHA, MATRIX, "TECH Output", "", "DeviceGray", Status::Fail),
            ("rearranged", "4020006080a0c0e0ff", ALPHA, MATRIX, "TECH Output", "", "DeviceGray", Status::Fail),
            ("alpha", SAMPLES, "ffffffffffffffff00", MATRIX, "TECH Output", "", "DeviceGray", Status::Fail),
            ("empty-alpha", SAMPLES, "000000000000000000", MATRIX, "TECH Output", "", "DeviceGray", Status::Fail),
            ("translation", SAMPLES, ALPHA, "36 0 0 36 19 18", "TECH Output", "", "DeviceGray", Status::Fail),
            ("scale", SAMPLES, ALPHA, "40 0 0 36 18 18", "TECH Output", "", "DeviceGray", Status::Fail),
            ("mirror", SAMPLES, ALPHA, "-36 0 0 36 54 18", "TECH Output", "", "DeviceGray", Status::Fail),
            ("layer", SAMPLES, ALPHA, MATRIX, "Wrong layer", "", "DeviceGray", Status::Fail),
            ("fractional", SAMPLES, "ffffffff80ffffffff", MATRIX, "TECH Output", "", "DeviceGray", Status::Incomplete),
            ("extra", SAMPLES, ALPHA, MATRIX, "TECH Output", "q 36 0 0 36 70 18 cm /Im Do Q", "DeviceGray", Status::Incomplete),
            ("mixed", SAMPLES, ALPHA, MATRIX, "TECH Output", "0 0 1 1 re f", "DeviceGray", Status::Incomplete),
            (
                "model",
                "000000000000000000000000000000000000000000000000000000000000000000000000",
                ALPHA,
                MATRIX,
                "TECH Output",
                "",
                "DeviceCMYK",
                Status::Fail,
            ),
        ] {
            let bytes = image(samples, alpha, matrix, layer, extra, model);
            let r = request(&bytes, &[]);
            check(name, &r, &bytes, status);
        }
    }
    #[test]
    fn approved_placement_is_replayed_and_never_fitted_to_output() {
        let bytes = image(SAMPLES, ALPHA, "-36 0 0 36 72 18", "TECH Output", "", "DeviceGray");
        // Physical x' = 31.75 - x, from the independently declared source transform.
        let edits = [fact(json!({"kind":"affine","regions":["page-0-image-0"],"matrix":[-1,0,0,1,31.75,0],"target_page":null}))];
        check("approved-mirror", &request(&bytes, &edits), &bytes, Status::Pass);
        let wrong = image(SAMPLES, ALPHA, MATRIX, "TECH Output", "", "DeviceGray");
        check("ignored-transform", &request(&wrong, &edits), &wrong, Status::Fail);
        let crop = [fact(json!({"kind":"crop","regions":["page-0-image-0"],"bounds_mm":[0,0,12,50]}))];
        check("crop", &request(&wrong, &crop), &wrong, Status::Incomplete);
    }
    #[test]
    fn unset_tolerance_stays_incomplete_but_known_sample_changes_fail() {
        for (name, samples, status) in
            [("criterion-unset", SAMPLES, Status::Incomplete), ("criterion-unset-tone", "1120406080a0c0e0ff", Status::Fail)]
        {
            let bytes = image(samples, ALPHA, MATRIX, "TECH Output", "", "DeviceGray");
            let mut r = request(&bytes, &[]);
            r.acceptance.remove("geometry_tolerance_mm");
            refresh(&mut r);
            check(name, &r, &bytes, status);
        }
    }
    #[test]
    fn stale_measurement_identity_never_authorizes_tolerance() {
        for (name, samples, status) in
            [("stale-criterion-shift", SAMPLES, Status::Incomplete), ("stale-criterion-tone", "1120406080a0c0e0ff", Status::Fail)]
        {
            let bytes = image(samples, ALPHA, "36 0 0 36 19 18", "TECH Output", "", "DeviceGray");
            let mut r = request(&bytes, &[]);
            r.acceptance.insert("geometry_tolerance_mm".into(), fact(json!(1.0)));
            check(name, &r, &bytes, status);
        }
    }
    #[test]
    fn lossless_row_column_and_transpose_reencodings_preserve_printed_samples() {
        for (name, values, matrix) in [
            ("equivalent-flip-x", "402000a08060ffe0c0", "-36 0 0 36 54 18"),
            ("equivalent-flip-y", "c0e0ff6080a0002040", "36 0 0 -36 18 54"),
            ("equivalent-transpose", "0060c02080e040a0ff", "0 -36 -36 0 54 54"),
        ] {
            let bytes = image(values, ALPHA, matrix, "TECH Output", "", "DeviceGray");
            check(name, &request(&bytes, &[]), &bytes, Status::Pass);
        }
        let bytes = image(SAMPLES, ALPHA, "0 36 -36 0 126 36", "TECH Output", "", "DeviceGray");
        let edit = fact(json!({"kind":"affine","regions":["page-0-image-0"],"matrix":[0,-1,1,0,0,44.45],"target_page":null}));
        check("approved-quarter-turn", &request(&bytes, &[edit]), &bytes, Status::Pass);
    }
    #[test]
    fn approved_oblique_rotation_retains_source_grid_without_fitting() {
        let c = std::f64::consts::FRAC_1_SQRT_2;
        let matrix = format!("{} {} {} {} 72 36", 36.0 * c, 36.0 * c, -36.0 * c, 36.0 * c);
        let bytes = image(SAMPLES, ALPHA, &matrix, "TECH Output", "", "DeviceGray");
        let edit = fact(
            json!({"kind":"affine","regions":["page-0-image-0"],"matrix":[c,-c,c,c,(72.0-144.0*c)*25.4/72.0,(108.0-108.0*c)*25.4/72.0],"target_page":null}),
        );
        check("approved-oblique-turn", &request(&bytes, &[edit]), &bytes, Status::Pass);
    }
    #[test]
    fn invisible_colour_and_padding_changes_are_not_print_defects() {
        let bytes = image("0020406011a0c0e0ff", ALPHA, MATRIX, "TECH Output", "", "DeviceGray");
        check("hidden-tone", &request(&bytes, &[]), &bytes, Status::Pass);
        let source = image(SAMPLES, "00000000ff00000000", MATRIX, "Source", "", "DeviceGray");
        // Move the occupied cell one column left, and its grid one cell right.
        // Printed sample, position and alpha are unchanged; only padding differs.
        let output = image("000000800000000000", "000000ff0000000000", "36 0 0 36 30 18", "TECH Output", "", "DeviceGray");
        let r = request_for_original(&source, &output, &[]);
        check_source("equivalent-padding", &r, &output, &source, Status::Pass);
        let wrong = image("000000810000000000", "000000ff0000000000", "36 0 0 36 30 18", "TECH Output", "", "DeviceGray");
        check_source("padding-tone-defect", &request_for_original(&source, &wrong, &[]), &wrong, &source, Status::Fail);
    }
}

#[cfg(test)]
#[path = "raster_fidelity_clip.rs"]
mod clip_tests;
