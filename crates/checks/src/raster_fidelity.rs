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
fn supported(paint: &Value) -> bool {
    matches!(paint.pointer("/description/colour_space").and_then(Value::as_str), Some("DeviceGray" | "DeviceCMYK"))
        && paint.get("sample_sha256").and_then(Value::as_str).is_some_and(|s| s.len() == 64)
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
fn oriented_hash(samples: &vectorcraft_pdf::PrintImageSamples, width: u32, height: u32, grid: Affine) -> Result<String, String> {
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
            let tone = if samples.alpha(at)? == 0.0 { 0.0 } else { samples.tone(at, channel)? };
            hash.update(tone.to_be_bytes());
        }
    }
    for pixel in 0..pixels {
        hash.update(samples.alpha(index(pixel)?)?.to_be_bytes());
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
    let hash = oriented_hash(samples, w, h, Affine::translate((f64::from(x), f64::from(y))))?;
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
    if order.layers.iter().filter(|l| *l == &link.pdf_layer).count() > 1 {
        return Err("Duplicate output layer identities prevent raster route reconciliation".into());
    }
    let tolerance = request
        .acceptance
        .get("geometry_tolerance_mm")
        .filter(|f| f.evidenced() && crate::digest(&request.acceptance).ok().as_ref() == Some(&request.identity.measurement_sha256))
        .and_then(|f| f.value.as_f64())
        .filter(|n| n.is_finite() && *n >= 0.0);
    let samples = image.samples.as_ref().ok_or("Observed normalized raster samples missing")?;
    let model_agrees = geometry.paint["colourants"] == actual.paint["colourants"]
        && geometry.paint.pointer("/description/colour_space") == actual.paint.pointer("/description/colour_space");
    let mut matched = vec![];
    if model_agrees {
        for grid in grids {
            let grid = Affine::new(grid);
            let samples_grid = Affine::translate((f64::from(actual_window[0]), f64::from(actual_window[1]))) * grid;
            let hash = oriented_hash(samples, expected_grid.width, expected_grid.height, samples_grid)?;
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
    let layer_agrees = layer == Some(&link.pdf_layer);
    let status = if !samples_agree || !page_agrees || !layer_agrees || tolerance.is_some_and(|t| matched.iter().all(|(d, e, _)| (d - e).max(0.0) > t))
    {
        Status::Fail
    } else if tolerance.is_some_and(|t| matched.iter().any(|(d, e, _)| d + e <= t)) {
        Status::Pass
    } else {
        Status::Incomplete
    };
    Ok((
        status,
        json!({"scope":"single_preserved_binary_alpha_image","source_region":id,"file_link":key,
        "samples_agree":samples_agree,"page_agrees":page_agrees,"layer_agrees":layer_agrees,
        "expected_visible_sample_sha256":expected_hash,"expected_visible_window":window,"observed_visible_window":actual_window,
        "expected_sample_sha256":geometry.paint["sample_sha256"],"observed_sample_sha256":actual.paint["sample_sha256"],
        "expected_pixel_to_page_mm":placed.pixel_to_page_mm,"observed_pixel_to_page_mm":actual.mask.pixel_to_page_mm,
        "max_corner_displacement_mm":best.map(|v|v.0),"numerical_error_mm":best.map(|v|v.1),"sample_grid_orientation":best.map(|v|v.2),"matching_orientations":matched.len(),"acceptance_mm":tolerance,
        "source_original_replay":"separate_required_obligation"}),
    ))
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
        let content = format!("/OC /Layer BDC q {matrix} cm /Im Do Q {extra} EMC");
        crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [7 0 R] /D << /Order [7 0 R] /ON [7 0 R] >> >> >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 144 144] /Resources << /Properties << /Layer 7 0 R >> /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>".into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            format!("<< /Type /XObject /Subtype /Image /Width 3 /Height 3 /BitsPerComponent 8 /ColorSpace /{model} /SMask 6 0 R /Filter /ASCIIHexDecode /Length {} >>\nstream\n{samples}>\nendstream",samples.len()+1),
            format!("<< /Type /XObject /Subtype /Image /Width 3 /Height 3 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode /Length {} >>\nstream\n{alpha}>\nendstream",alpha.len()+1),
            format!("<< /Type /OCG /Name ({layer}) >>"),
        ])
    }
    const SAMPLES: &str = "0020406080a0c0e0ff";
    const ALPHA: &str = "ffffffff00ffffffff";
    const MATRIX: &str = "36 0 0 36 18 18";
    fn original() -> Vec<u8> {
        image(SAMPLES, ALPHA, MATRIX, "Source", "", "DeviceGray")
    }
    fn request(bytes: &[u8], edits: &[Fact]) -> Request {
        request_for_original(&original(), bytes, edits)
    }
    fn request_for_original(original: &[u8], bytes: &[u8], edits: &[Fact]) -> Request {
        let mut r = crate::attribution::tests::request();
        let draft = crate::prepare::capture(original, "synthetic".into(), r.identity.job_revision.clone()).unwrap();
        let (id, source) = draft.raster_sources.first_key_value().unwrap();
        let mut decisions =
            vec![fact(json!({"kind":"raster_mask","source":id,"source_sha256":source.mask.source_sha256,"policy":"exact_binary_alpha"}))];
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
    fn check_source(name: &str, r: &Request, bytes: &[u8], original: &[u8], expected: Status) {
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
