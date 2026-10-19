//! Final binary ink coverage, reconstructed in raw PDF paint order. Layer-local
//! unions are not physical plates: later knockout can remove earlier ink.
use crate::pdf::Census;
use vectorcraft_geom::{FillRule, PathData};
use vectorcraft_pathops::BoolOp;

pub(crate) struct Coverage {
    pub path: PathData,
    pub construction_allowance_mm: f64,
    pub paint_object_ids: Vec<u64>,
}
fn elements(path: &PathData) -> usize {
    path.anchor_count().saturating_add(path.subpaths.len().saturating_mul(2))
}
fn boolean(a: &PathData, b: &PathData, op: BoolOp) -> Result<PathData, String> {
    let out = vectorcraft_pathops::try_boolean_for_measurement(a, FillRule::NonZero, b, FillRule::NonZero, op).map_err(|e| e.to_string())?;
    if elements(&out) > 100_000 {
        return Err("Physical coverage output geometry budget exceeded".into());
    }
    Ok(out)
}
/// Shared completeness gate for physical presence, absence and comparison.
/// Object inventory alone cannot establish the result after PDF compositing.
pub(crate) fn reconciled_vector_census(census: &Census) -> Result<(), String> {
    if census.pages == 0
        || !census.paint_order_verified
        || census.budget_exhausted
        || census.image_objects != 0
        || !census.unresolved.is_empty()
        || !census.unresolved_layers.is_empty()
        || !census.unaccounted_objects.is_empty()
        || census.raw_audit_error.is_some()
        || census.raw_audit.as_ref().is_none_or(|a| a.issue_count != 0 || a.image_use_count != 0 || a.path_paint_count != census.paints.len())
    {
        return Err("Complete reconciled vector census is required for physical coverage".into());
    }
    Ok(())
}
/// Requires complete supported vector accounting. No threshold converts a partial
/// tint into solid ink; unsupported channels, masks and colour conversion refuse.
#[cfg(test)]
pub(crate) fn solid(census: &Census, page: usize, ink: &str) -> Result<Coverage, String> {
    solid_with_budget(census, page, ink, &mut crate::feature_offset::WorkBudget::new())
}
pub(crate) fn solid_with_budget(census: &Census, page: usize, ink: &str, budget: &mut crate::feature_offset::WorkBudget) -> Result<Coverage, String> {
    if page >= census.pages || ink.is_empty() || matches!(ink, "All" | "None") {
        return Err("Physical coverage page or ink is invalid".into());
    }
    reconciled_vector_census(census)?;
    let mut path = PathData::default();
    let mut construction_allowance_mm = 0.0f64;
    let mut cost = 0usize;
    let mut count = 0usize;
    let mut paint_object_ids = std::collections::BTreeSet::new();
    for paint in &census.paints {
        let source = paint.source_space.and_then(|i| census.paint_sources.get(i)).ok_or("Physical coverage source identity missing")?;
        let tones = crate::tone::source_tones(paint, source)?;
        if tones.values().any(|t| !t.is_finite() || ![0.0, 1.0].contains(t)) {
            return Err("Physical feature coverage requires binary solid ink; partial tone needs a separate approved criterion".into());
        }
        let painted_page = paint.page.ok_or("Physical coverage page unresolved")?;
        let shape = paint.geometry_mm.as_ref().ok_or("Physical coverage geometry unresolved")?;
        if painted_page >= census.pages || !paint.geometry_error_mm.is_finite() || paint.geometry_error_mm < 0.0 {
            return Err("Physical coverage page or uncertainty invalid".into());
        }
        if painted_page != page || shape.is_empty() {
            continue;
        }
        budget.charge(elements(shape).saturating_add(elements(&path)))?;
        count += 1;
        cost = cost.saturating_add(elements(shape)).saturating_add(elements(&path));
        if count > 1000 || cost > 500_000 {
            return Err("Physical coverage composition budget exceeded".into());
        }
        let Some(tone) = crate::tone::overpaint(&tones, ink, paint.overprint, crate::tone::skip_zero_process(paint, source)) else {
            continue;
        };
        paint_object_ids.insert(paint.id);
        let sweep = vectorcraft_pathops::measurement_boolean_epsilon(&path, shape).map_err(|e| e.to_string())?;
        path = boolean(&path, shape, if tone == 1.0 { BoolOp::Union } else { BoolOp::Difference })?;
        // Retain input error and a Boolean construction allowance (not a certified enclosure); never use it as
        // a manufacturing tolerance or suppress a small component.
        construction_allowance_mm += paint.geometry_error_mm + 2.0 * sweep;
    }
    Ok(Coverage { path, construction_allowance_mm, paint_object_ids: paint_object_ids.into_iter().collect() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_geom::{Point, Shape};
    fn coverage(extra: &str, mode: u8, ink: &str) -> Result<Coverage, String> {
        let bytes = crate::pdf::tests::fixture_mode(true, mode, "Exact#20ink", extra, None);
        let doc = vectorcraft_pdf::import(&bytes).unwrap();
        let mut census = crate::pdf::census(&doc);
        crate::ordering::try_reconcile(&bytes, &mut census)?;
        solid(&census, 0, ink)
    }
    fn contains(c: &Coverage, x: f64, y: f64) -> bool {
        c.path.to_bezpath().winding(Point::new(x * 25.4 / 72.0, (100.0 - y) * 25.4 / 72.0)) != 0
    }
    #[test]
    fn final_coverage_obeys_named_zero_and_later_knockout() {
        let c = coverage("0 scn 15 15 5 5 re f", 1, "Exact ink").unwrap();
        assert!(contains(&c, 12.0, 12.0));
        assert!(!contains(&c, 17.0, 17.0));
        let c = coverage("0 0 0 0 k 15 15 5 5 re f", 1, "Exact ink").unwrap();
        assert!(contains(&c, 17.0, 17.0), "overprinting process paint preserves unmentioned spot");
        let bytes = crate::pdf::tests::fixture(false, "Exact#20ink", "0 0 0 0 k 15 15 5 5 re f", None);
        let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        crate::ordering::try_reconcile(&bytes, &mut census).unwrap();
        assert!(!contains(&solid(&census, 0, "Exact ink").unwrap(), 17.0, 17.0));
    }
    #[test]
    fn device_cmyk_zero_depends_on_overprint_mode() {
        for (mode, expected) in [(0, false), (1, true)] {
            let c = coverage("0 0 0 0 k 0 0 50 50 re f", mode, "Cyan").unwrap();
            assert_eq!(contains(&c, 5.0, 5.0), expected);
        }
    }
    #[test]
    fn partial_tone_and_unreconciled_census_cannot_prove_binary_features() {
        assert!(coverage("0.5 scn 15 15 5 5 re f", 1, "Exact ink").is_err());
        let mut census = crate::pdf::census(&vectorcraft_pdf::import(&crate::pdf::tests::pdf(true)).unwrap());
        assert!(solid(&census, 0, "Exact ink").is_err());
        crate::ordering::try_reconcile(&crate::pdf::tests::pdf(true), &mut census).unwrap();
        assert!(solid(&census, 1, "Exact ink").is_err());
        census.unaccounted_objects.push("unresolved".into());
        assert!(solid(&census, 0, "Exact ink").is_err());
    }
    #[test]
    fn differences_retain_small_holes_and_narrow_remaining_ink() {
        // The editable PDF importer substitutes a stroke for this sub-threshold fill.
        // Refuse that mismatch instead of claiming its physical coverage is known.
        assert!(coverage("0 scn 15 15 0.0001 0.0001 re f", 1, "Exact ink").is_err());
        let hole = coverage("0 scn 15 15 0.1 0.1 re f", 1, "Exact ink").unwrap();
        assert_eq!(hole.path.subpaths.len(), 2);
        assert!(!contains(&hole, 15.05, 15.05));
        let remnant = coverage("0 scn 10 10 19.9999 20 re f", 1, "Exact ink").unwrap();
        assert!(!remnant.path.is_empty());
        assert!(contains(&remnant, 29.99995, 15.0));
        let edge = coverage("0 scn 20 10 10 20 re f", 1, "Exact ink").unwrap();
        assert_eq!(edge.path.subpaths.len(), 1);
        assert!(contains(&edge, 15.0, 15.0));
        assert!(!contains(&edge, 25.0, 15.0));
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod independent_tests {
    use super::*;
    #[test]
    #[ignore = "Requires CHECK_GS_BINARY; run explicitly with --include-ignored for independent qualification"]
    fn physical_coverage_matches_actual_independent_spot_and_process_planes() {
        let binary = std::env::var("CHECK_GS_BINARY").expect("Independent qualification requires explicit CHECK_GS_BINARY");
        let base = std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let run = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let mut evidence = vec![];
        let mut cases: Vec<_> = [
            ("spot-zero", true, 1, "0 scn 15 15 5 5 re f", "Exact ink"),
            ("spot-knockout", false, 1, "0 0 0 0 k 15 15 5 5 re f", "Exact ink"),
            ("process-opm0", true, 0, "0 0 0 0 k 0 0 50 50 re f", "Cyan"),
            ("process-opm1", true, 1, "0 0 0 0 k 0 0 50 50 re f", "Cyan"),
        ]
        .into_iter()
        .map(|(name, op, mode, extra, ink)| (name, crate::pdf::tests::fixture_mode(op, mode, "Exact#20ink", extra, None), ink))
        .collect();
        for (name, second, overprint, second_page, reverse) in [
            ("stream-full-knockout", "0 0 0 0 k 0 0 100 100 re f", false, false, false),
            ("stream-partial-knockout", "0 0 0 0 k 15 15 5 5 re f", false, false, false),
            ("other-spot-knockout", "/Other cs 1 scn 0 0 100 100 re f", false, false, false),
            ("other-spot-overprint", "/Other cs 1 scn 0 0 100 100 re f", true, false, false),
            ("knockout-before-ink", "0 0 0 0 k 0 0 100 100 re f", false, false, true),
            ("different-page-knockout", "0 0 0 0 k 0 0 100 100 re f", false, true, false),
        ] {
            cases.push((name, crate::pdf::tests::ordered_ink_fixture(second, overprint, 1, second_page, reverse), "Exact ink"));
        }
        for (name, bytes, ink) in cases {
            let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
            crate::ordering::try_reconcile(&bytes, &mut census).unwrap();
            let coverage = solid(&census, 0, ink).unwrap();
            let boundary = crate::geometry::Boundary::new(&coverage.path, FillRule::NonZero, 0.000001).unwrap();
            let config = crate::independent::Configuration {
                binary: binary.clone().into(),
                binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                version: "10.07.1".into(),
                dpi: 144,
                timeout_ms: 10_000,
                max_output_bytes: 16 * 1024 * 1024,
                qualification_sha256: None,
            };
            let output = base.join(format!("feature-coverage-{name}-{run}"));
            let rendering = crate::independent::render(&config, &bytes, &output).unwrap();
            let plane = rendering.planes.iter().find(|p| p.page == 1 && p.name == ink).unwrap();
            let image = crate::raster::read_plane(plane).unwrap();
            let mut compared = 0;
            let mut wrong = 0;
            let mut budget = 0;
            for (x, y, pixel) in image.enumerate_pixels() {
                let p =
                    vectorcraft_geom::Point::new((f64::from(x) + 0.5) * rendering.pixel_pitch_mm, (f64::from(y) + 0.5) * rendering.pixel_pitch_mm);
                if boundary.near_boundary(p, rendering.pixel_pitch_mm * 2.0f64.sqrt() + coverage.construction_allowance_mm, &mut budget).unwrap() {
                    continue;
                }
                let expected = if boundary.contains(p, FillRule::NonZero) { 0 } else { 255 };
                compared += 1;
                if pixel.0[0] != expected {
                    wrong += 1;
                }
            }
            assert!(compared > 1000);
            assert_eq!(wrong, 0, "{name}");
            evidence.push(serde_json::json!({"case":name,"pdf_sha256":crate::sha256(&bytes),"configuration_sha256":rendering.configuration_sha256,"plane_sha256":plane.sha256,"pixels_compared":compared,"wrong_pixels":wrong}));
            if std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR").is_none() {
                std::fs::remove_dir_all(output).unwrap();
            }
        }
        if std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR").is_some() {
            std::fs::write(
                base.join(format!("coverage-render-{run}.json")),
                serde_json::to_vec_pretty(&serde_json::json!({"engine":crate::ENGINE_VERSION,"cases":evidence})).unwrap(),
            )
            .unwrap();
        }
    }
}
