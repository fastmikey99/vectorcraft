//! Compare combined physical output geometry in exact exported PDF page space.
use crate::{Evidence, Location, Report, Request, Status, attribution::Plan, geometry, pdf::Census, program};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use vectorcraft_geom::Shape;
fn evidence(request: &Request) -> Vec<Evidence> {
    vec![
        Evidence {
            artifact_sha256: request.identity.pdf_sha256.clone(),
            producer: "checks/pdf-boundary-1".into(),
            detail: "Fresh public PDF geometry in page-local mm; independent boundary maximum measurement".into(),
        },
        Evidence {
            artifact_sha256: request.identity.baseline_sha256.clone(),
            producer: "checks/prepared-geometry-1".into(),
            detail: "Geometry artifacts checked against approved baseline hashes".into(),
        },
    ]
}
fn record_physical(
    request: &Request,
    report: &mut Report,
    key: &str,
    links: &[&str],
    status: Status,
    reason: &str,
    observed: Value,
) -> Result<(), String> {
    for key in std::iter::once(format!("plate/{key}")).chain(links.iter().map(|l| format!("file/{l}/coverage"))) {
        if report.findings.iter().any(|f| f.obligation.key == key && f.status == Status::NotApplicable) {
            continue;
        }
        report.record(&key, status, reason, observed.clone(), evidence(request))?;
    }
    Ok(())
}
fn exact_expected(pieces: &[&program::Piece], ink: &str) -> Result<(), String> {
    if ["All", "None", "Cyan", "Magenta", "Yellow", "Black"].contains(&ink) {
        return Err("Special/process exact plate names require explicit physical-channel reconciliation".into());
    }
    for piece in pieces {
        let output_ink = piece.paint.get("mode").and_then(Value::as_str) == Some("output_ink");
        let solid = piece.paint.get("type").and_then(Value::as_str) == Some("solid");
        if piece.ink.as_deref() != Some(ink)
            || (!output_ink && !solid)
            || crate::tone::tones(&piece.paint, piece.ink.as_deref())?.get(ink) != Some(&1.0)
        {
            return Err("Exact physical plate expectations require matching, binary-solid named-ink pieces".into());
        }
    }
    Ok(())
}
pub fn check(request: &Request, plan: &Plan, census: &Census, report: &mut Report) -> Result<(), String> {
    let program = match program::execute(request, plan) {
        Ok(p) => p,
        Err(reason) => {
            report.record("global/fidelity", Status::Incomplete, reason, Value::Null, vec![])?;
            return Ok(());
        }
    };
    crate::paint_fidelity::check(request, &program, census, report)?;
    for (key, reason) in &program.unresolved {
        report.record(&format!("rule/{key}/geometry"), Status::Incomplete, reason.clone(), Value::Null, vec![])?;
    }
    let tolerance = request
        .acceptance
        .get("geometry_tolerance_mm")
        .filter(|f| f.evidenced())
        .and_then(|f| f.value.as_f64())
        .filter(|n| n.is_finite() && *n >= 0.0);
    let accepted_identity = crate::digest(&request.acceptance)? == request.identity.measurement_sha256;
    let plates = request.recipe.foundation.as_ref().and_then(|f| f.pointer("/verification/plates")).and_then(Value::as_array);
    let Some(plates) = plates else {
        return Ok(());
    };
    let mut any_failure = false;
    let mut all_geometry = true;
    let mut coverage_budget = crate::feature_offset::WorkBudget::new();
    for plate in plates.iter().filter(|p| p.get("garment").and_then(Value::as_str).is_some_and(|g| request.garment.matches(g))) {
        let key = plate.get("key").and_then(Value::as_str).ok_or("Plate key missing")?;
        let links: Vec<_> = plate.get("links").and_then(Value::as_array).ok_or("Plate links missing")?.iter().filter_map(Value::as_str).collect();
        let ready = links.iter().all(|k| program.outputs.contains_key(*k));
        let pieces: Vec<_> = links.iter().flat_map(|k| program.outputs.get(*k).into_iter().flatten()).collect();
        let ink = plate.get("name").and_then(Value::as_str);
        let required = plate.get("required").and_then(Value::as_bool) == Some(true);
        let basis = "opaque binary named ink reconstructed in complete raw PDF paint order";
        let reconstructed = (|| -> Result<_, String> {
            crate::coverage::reconciled_vector_census(census)?;
            if plate.get("name_mode").and_then(Value::as_str) != Some("exact") {
                return Err(
                    "Process and source destinations require qualified positive-support reconstruction and an explicit expected channel model".into(),
                );
            }
            if !ready {
                return Err("Complete expected contribution geometry is required".into());
            }
            let ink = ink.filter(|s| !s.is_empty()).ok_or("Exact physical ink name missing")?;
            exact_expected(&pieces, ink)?;
            let mut pages: BTreeSet<_> =
                pieces.iter().map(|p| p.page).chain(census.paints.iter().filter(|p| p.ink.as_deref() == Some(ink)).filter_map(|p| p.page)).collect();
            // Even an apparently absent ink needs full tonal/census validation;
            // unsupported named channels must not manufacture an empty-plate Fail.
            if pages.is_empty() {
                pages.insert(0);
            }
            let mut result = std::collections::BTreeMap::new();
            for page in pages {
                result.insert(page, crate::coverage::solid_with_budget(census, page, ink, &mut coverage_budget)?);
            }
            Ok(result)
        })();
        let reconstructed = match reconstructed {
            Ok(value) => value,
            Err(reason) => {
                record_physical(request, report, key, &links, Status::Incomplete, &reason, json!({"basis":basis}))?;
                all_geometry = false;
                continue;
            }
        };
        let printable = reconstructed.values().any(|c| !c.path.is_empty());
        let reconstruction = json!({"basis":basis,"pages_examined":reconstructed.keys().collect::<Vec<_>>(),
            "other_pages":"no expected piece or named-ink paint; complete global tonal census validated",
            "paint_object_ids":reconstructed.values().flat_map(|c| &c.paint_object_ids).copied().collect::<BTreeSet<_>>()});
        if required && !printable {
            record_physical(
                request,
                report,
                key,
                &links,
                Status::Fail,
                "Unconditionally required physical plate is empty after ordered ink compositing",
                reconstruction,
            )?;
            let page = pieces.iter().find(|p| !p.path.is_empty()).map(|p| p.page).or_else(|| reconstructed.keys().next().copied()).unwrap_or(0);
            let bounds_mm = pieces.iter().find(|p| p.page == page && !p.path.is_empty()).map(|p| {
                let b = p.path.to_bezpath().bounding_box();
                [b.x0, b.y0, b.x1, b.y1]
            });
            let location = Location {
                page,
                bounds_mm,
                object_ids: reconstructed.get(&page).map(|c| c.paint_object_ids.iter().take(64).copied().collect()).unwrap_or_default(),
            };
            for finding in &mut report.findings {
                if finding.status == Status::Fail
                    && (finding.obligation.key == format!("plate/{key}")
                        || links.iter().any(|l| finding.obligation.key == format!("file/{l}/coverage")))
                {
                    finding.location = Some(location.clone());
                }
            }
            any_failure = true;
            all_geometry = false;
            continue;
        }
        let named_objects = census.paints.iter().any(|p| p.ink.as_deref() == ink);
        if pieces.iter().all(|p| p.path.is_empty()) && !printable && !required && !named_objects {
            // Plate optionality does not imply contribution optionality. Attribution
            // alone waives conditional links; unconditional link findings stay visible.
            report.record(
                &format!("plate/{key}"),
                Status::NotApplicable,
                "All contributions proven empty and complete physical census contains no named-ink paint",
                reconstruction,
                evidence(request),
            )?;
            continue;
        }
        if tolerance.is_none() || !accepted_identity {
            report.record(
                &format!("plate/{key}"),
                Status::Incomplete,
                "Approved geometric acceptance is required for measured conformity",
                reconstruction.clone(),
                evidence(request),
            )?;
            all_geometry = false;
            continue;
        }
        let tolerance = tolerance.unwrap_or(0.0);
        let mut measurements = vec![];
        let mut status = Status::Pass;
        let mut location = None;
        let mut uncertainty = 0.0f64;
        for (page, coverage) in &reconstructed {
            let page = *page;
            let expected = program::union(pieces.iter().filter(|p| p.page == page).map(|p| p.path.clone()))?;
            let observed = &coverage.path;
            let construction_error =
                pieces.iter().filter(|p| p.page == page).map(|p| p.construction_error_mm).fold(0.0, f64::max) + coverage.construction_allowance_mm;
            let precision = (tolerance / 16.0).clamp(0.000001, 0.001);
            match geometry::compare(&expected, observed, tolerance, precision) {
                Ok(mut m) => {
                    m.lower_mm = (m.lower_mm - construction_error).max(0.0);
                    m.upper_mm += construction_error;
                    m.uncertainty_mm = m.upper_mm - m.lower_mm;
                    if m.expected_components != m.observed_components || m.expected_holes != m.observed_holes {
                        m.status = Status::Fail;
                    } else if m.status != Status::Fail || m.lower_mm <= tolerance {
                        m.status = if m.lower_mm > tolerance {
                            Status::Fail
                        } else if m.upper_mm <= tolerance {
                            Status::Pass
                        } else {
                            Status::Incomplete
                        };
                    }
                    if m.status == Status::Fail {
                        status = Status::Fail;
                    } else if m.status == Status::Incomplete && status != Status::Fail {
                        status = Status::Incomplete;
                    }
                    if location.is_none() || m.status == Status::Fail {
                        let b = expected.to_bezpath().bounding_box().union(observed.to_bezpath().bounding_box());
                        let bounds = m.witness_mm.map(|[x, y]| [x - 1.0, y - 1.0, x + 1.0, y + 1.0]).unwrap_or([b.x0, b.y0, b.x1, b.y1]);
                        location = Some(Location {
                            page,
                            bounds_mm: Some(bounds),
                            object_ids: coverage.paint_object_ids.iter().take(64).copied().collect(),
                        });
                    }
                    uncertainty = uncertainty.max(m.uncertainty_mm);
                    measurements.push(json!({"page":page,"measurement":m,"coverage_construction_allowance_mm":coverage.construction_allowance_mm,"paint_object_ids":coverage.paint_object_ids}));
                }
                Err(reason) => {
                    if status != Status::Fail {
                        status = Status::Incomplete;
                    }
                    measurements.push(json!({"page":page,"unresolved":reason}));
                }
            }
        }
        let mut keys = vec![format!("plate/{key}")];
        keys.extend(links.iter().map(|l| format!("file/{l}/coverage")));
        for k in keys {
            // Source-proven absence remains a contribution fact even when another
            // contribution on the same physical plate prints successfully.
            if report.findings.iter().any(|f| f.obligation.key == k && f.status == Status::NotApplicable) {
                continue;
            }
            report.record(
                &k,
                status,
                "Final ordered binary physical ink coverage compared against the complete recipe graph",
                json!({"acceptance_mm":tolerance,"measurements":measurements,"reconstruction":reconstruction}),
                evidence(request),
            )?;
            if let Some(f) = report.findings.iter_mut().find(|f| f.obligation.key == k) {
                f.location = location.clone();
                f.uncertainty_mm = Some(uncertainty);
            }
        }
        any_failure |= status == Status::Fail;
        all_geometry &= status == Status::Pass;
    }
    crate::contribution::check(request, &program, report)?;
    report.record(
        "global/file_structure",
        if any_failure { Status::Fail } else { Status::Incomplete },
        if all_geometry {
            "Opaque binary named-ink coverage follows verified paint order; independent interpreter and broader print semantics remain separate"
        } else {
            "Physical output coverage findings retained with unresolved areas"
        },
        json!({"all_physical_geometry_passed":all_geometry}),
        evidence(request),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        preparation::{Fact, Provenance},
        program::{Inputs, RegionGeometry},
    };
    use std::collections::BTreeMap;
    fn request(bytes: &[u8]) -> Request {
        let mut r = crate::attribution::tests::request();
        r.recipe.rules[0].operation = "preserve".into();
        r.recipe.rules[0].distance_mm = None;
        let k = 25.4 / 72.0;
        let mut p = vectorcraft_geom::BezPath::new();
        p.move_to((10.0 * k, 70.0 * k));
        p.line_to((30.0 * k, 70.0 * k));
        p.line_to((30.0 * k, 90.0 * k));
        p.line_to((10.0 * k, 90.0 * k));
        p.close_path();
        let geometry = RegionGeometry {
            prepared_mask: None,
            page: 0,
            path_mm: vectorcraft_geom::PathData::from_bezpath(&p),
            fill_rule: vectorcraft_geom::FillRule::NonZero,
            paint: json!({"mode":"output_ink"}),
            geometry_error_mm: 0.0,
            source_order: None,
        };
        r.baseline.as_mut().unwrap().regions[0].geometry_sha256 = crate::digest(&geometry).unwrap();
        r.geometry = Some(Inputs { regions: BTreeMap::from([("region-1".into(), geometry)]) });
        r.acceptance.insert(
            "geometry_tolerance_mm".into(),
            Fact {
                value: json!(0.02),
                provenance: Provenance::Operator {
                    actor: "Synthetic fixture author".into(),
                    at: "2026-10-18".into(),
                    decision_id: "Synthetic criterion only".into(),
                    evidence_sha256: "e".repeat(64),
                },
            },
        );
        r.identity.baseline_sha256 = crate::digest(r.baseline.as_ref().unwrap()).unwrap();
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        r.identity.measurement_sha256 = crate::digest(&r.acceptance).unwrap();
        r.identity.pdf_sha256 = crate::sha256(bytes);
        r
    }
    #[test]
    fn exact_pdf_geometry_is_measured_and_extra_islands_get_locations() {
        for (extra, status) in [("", Status::Pass), ("60 60 1 1 re f", Status::Fail)] {
            let bytes = crate::pdf::tests::fixture(true, "Exact#20ink", extra, None);
            let r = request(&bytes);
            let report = crate::pdf::check(&r, &bytes).unwrap();
            let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
            assert_eq!(f.status, status, "{}", serde_json::to_string_pretty(&report).unwrap());
            assert!(f.location.is_some());
            assert!(f.uncertainty_mm.is_some());
            for key in ["rule/arbitrary/geometry", "rule/arbitrary/topology", "file/output/contribution"] {
                assert_eq!(report.findings.iter().find(|f| f.obligation.key == key).unwrap().status, status, "{key}");
            }
            assert_eq!(report.findings.iter().find(|f| f.obligation.key == "rule/arbitrary/protection").unwrap().status, Status::NotApplicable);
            assert_ne!(report.status, Status::Pass);
        }
    }
    #[test]
    fn source_and_composite_destinations_do_not_claim_physical_support_from_object_outlines() {
        for mode in ["source", "composite"] {
            let bytes = crate::pdf::tests::pdf(true);
            let mut r = request(&bytes);
            let destination = if mode == "composite" { "Indigo" } else { "Screen" };
            r.recipe.required_screens = Some(vec![]);
            r.recipe.file_links[0].name_mode = "source".into();
            r.recipe.file_links[0].separation_name = None;
            r.recipe.file_links[0].destination = destination.into();
            let foundation = r.recipe.foundation.as_mut().unwrap();
            foundation["treatments"]["arbitrary"]["paint"] = json!({"mode":"source"});
            for p in foundation["verification"]["plates"].as_array_mut().unwrap() {
                p["name_mode"] = json!(mode);
                p["name"] = Value::Null;
                p["required"] = json!(false);
                p["destination"] = json!(destination);
            }
            r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
            assert!(r.recipe.contract_issues(r.garment).is_empty(), "{:?}", r.recipe.contract_issues(r.garment));
            crate::attribution::resolve(&r).unwrap();
            let report = crate::pdf::check(&r, &bytes).unwrap();
            assert_eq!(report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap().status, Status::Incomplete);
            {
                assert!(report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap().reason.contains("positive-support"));
            }
        }
    }
    #[test]
    fn missing_or_changed_acceptance_never_gets_a_geometry_pass() {
        let bytes = crate::pdf::tests::pdf(true);
        let mut r = request(&bytes);
        r.acceptance.clear();
        let report = crate::pdf::check(&r, &bytes).unwrap();
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap().status, Status::Incomplete);
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "rule/arbitrary/geometry").unwrap().status, Status::Incomplete);
    }
    fn merged(bytes: &[u8], empty_second: bool) -> Request {
        let mut r = request(bytes);
        let mut rule = r.recipe.rules[0].clone();
        rule.rule_key = "second".into();
        r.recipe.rules.push(rule);
        let mut link = r.recipe.file_links[0].clone();
        link.key = "second-output".into();
        link.source_rule = "second".into();
        r.recipe.file_links.push(link);
        let foundation = r.recipe.foundation.as_mut().unwrap();
        foundation["treatments"]["second"] = foundation["treatments"]["arbitrary"].clone();
        if empty_second {
            foundation["treatments"]["second"]["select"] = json!([[{"field":"coverage_exception","operator":"equals","value":true}]]);
        }
        foundation["outputs"]["second-output"] = foundation["outputs"]["output"].clone();
        foundation["verification"]["contributions"]["second-output"] = foundation["verification"]["contributions"]["output"].clone();
        for plate in foundation["verification"]["plates"].as_array_mut().unwrap() {
            plate["links"].as_array_mut().unwrap().push(json!("second-output"));
        }
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        assert!(r.recipe.contract_issues(r.garment).is_empty(), "{:?}", r.recipe.contract_issues(r.garment));
        r
    }
    #[test]
    fn legal_merged_contributions_pass_without_separate_objects_and_failures_are_not_misattributed() {
        for (extra, expected) in [("", Status::Pass), ("60 60 1 1 re f", Status::Incomplete)] {
            let bytes = crate::pdf::tests::fixture(true, "Exact#20ink", extra, None);
            let r = merged(&bytes, false);
            let report = crate::pdf::check(&r, &bytes).unwrap();
            for key in ["rule/arbitrary/geometry", "rule/second/geometry", "rule/second/topology", "file/second-output/contribution"] {
                assert_eq!(report.findings.iter().find(|f| f.obligation.key == key).unwrap().status, expected, "{key}: {report:?}");
            }
            if !extra.is_empty() {
                assert_eq!(report.status, Status::Fail);
                assert_eq!(report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap().status, Status::Fail);
            }
        }
    }
    #[test]
    fn printing_a_combined_plate_does_not_erase_a_proven_empty_contribution() {
        let bytes = crate::pdf::tests::pdf(true);
        let r = merged(&bytes, true);
        let report = crate::pdf::check(&r, &bytes).unwrap();
        for facet in ["contribution", "identity", "layer", "order", "overprint", "coverage"] {
            let key = format!("file/second-output/{facet}");
            assert_eq!(report.findings.iter().find(|f| f.obligation.key == key).unwrap().status, Status::NotApplicable, "{key}");
        }
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap().status, Status::Pass);
    }
    #[test]
    fn final_ink_geometry_obeys_knockout_zero_tint_and_overprint() {
        for (overprint, mode, extra, expected) in [
            (false, 1, "0 0 0 0 k 0 0 100 100 re f", Status::Fail),
            (false, 1, "0 0 0 0 k 15 15 5 5 re f", Status::Fail),
            (true, 0, "0 0 0 0 k 15 15 5 5 re f", Status::Pass),
            (true, 1, "0 0 0 0 k 15 15 5 5 re f", Status::Pass),
            (true, 1, "0 scn 15 15 5 5 re f", Status::Fail),
            (true, 1, "0 scn 10 10 20 20 re f", Status::Fail),
            (true, 1, "0.5 scn 15 15 5 5 re f", Status::Incomplete),
            (false, 1, "0 0 0 0 k 0 0 100 100 re f /Ink cs 1 scn 10 10 20 20 re f", Status::Pass),
        ] {
            let bytes = crate::pdf::tests::fixture_mode(overprint, mode, "Exact#20ink", extra, None);
            let r = request(&bytes);
            let report = crate::pdf::check(&r, &bytes).unwrap();
            let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
            assert_eq!(f.status, expected, "{overprint} {mode} {extra}: {f:?}");
            assert_ne!(report.status, Status::Pass);
            if f.reason == "Unconditionally required physical plate is empty after ordered ink compositing" {
                assert!(f.location.as_ref().is_some_and(|l| !l.object_ids.is_empty()));
                let topology = report.findings.iter().find(|f| f.obligation.key == "rule/arbitrary/topology").unwrap();
                assert_eq!(topology.status, Status::Incomplete);
                assert!(topology.reason.contains("topology was not measured"));
            }
            for key in ["file/output/coverage", "file/output/contribution", "rule/arbitrary/geometry"] {
                assert_eq!(report.findings.iter().find(|f| f.obligation.key == key).unwrap().status, expected, "{key}: {extra}");
            }
            if expected == Status::Fail && extra.contains("15 15") {
                let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
                crate::ordering::try_reconcile(&bytes, &mut census).unwrap();
                let knockout = census.paints.last().unwrap().id;
                assert!(f.location.as_ref().unwrap().object_ids.contains(&knockout), "specific knockout paint must be included in diagnostics");
            }
        }
    }
    #[test]
    fn zero_only_ink_and_incomplete_census_cannot_manufacture_presence_or_absence() {
        let bytes = String::from_utf8(crate::pdf::tests::pdf(true)).unwrap().replace("/Ink cs 1 scn", "/Ink cs 0 scn").into_bytes();
        let r = request(&bytes);
        let report = crate::pdf::check(&r, &bytes).unwrap();
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap().status, Status::Fail);
        for missing in [0, 1, 2] {
            let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
            crate::ordering::try_reconcile(&bytes, &mut census).unwrap();
            match missing {
                0 => census.paint_order_verified = false,
                1 => census.raw_audit.as_mut().unwrap().path_paint_count += 1,
                _ => census.unaccounted_objects.push("unresolved paint".into()),
            }
            let mut report = crate::begin(&r).unwrap();
            check(&r, &crate::attribution::resolve(&r).unwrap(), &census, &mut report).unwrap();
            assert_eq!(report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap().status, Status::Incomplete);
        }
    }
    #[test]
    fn nonbinary_expected_named_ink_cannot_be_compared_as_a_solid_shape() {
        let bytes = crate::pdf::tests::pdf(true);
        let mut r = request(&bytes);
        r.recipe.foundation.as_mut().unwrap()["treatments"]["arbitrary"]["paint"] = json!({"mode":"source"});
        let g = r.geometry.as_mut().unwrap().regions.get_mut("region-1").unwrap();
        g.paint = json!({"type":"solid","color":{"model":"cmyk","c":0.0,"m":1.0,"y":0.0,"k":0.0},"tint":0.5});
        r.baseline.as_mut().unwrap().regions[0].geometry_sha256 = crate::digest(g).unwrap();
        r.identity.baseline_sha256 = crate::digest(r.baseline.as_ref().unwrap()).unwrap();
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        let report = crate::pdf::check(&r, &bytes).unwrap();
        let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
        assert_eq!(f.status, Status::Incomplete);
        assert!(f.reason.contains("binary-solid"), "{}", f.reason);
    }
    #[test]
    fn separate_streams_and_pages_use_paint_order_not_layer_order() {
        for (second, overprint, second_page, reverse, expected) in [
            ("0 0 0 0 k 0 0 100 100 re f", false, false, false, Status::Fail),
            ("0 0 0 0 k 15 15 5 5 re f", false, false, false, Status::Fail),
            ("0 0 0 0 k 0 0 100 100 re f", true, false, false, Status::Pass),
            ("/Other cs 1 scn 0 0 100 100 re f", false, false, false, Status::Fail),
            ("/Other cs 1 scn 0 0 100 100 re f", true, false, false, Status::Pass),
            ("/Ink cs 0 scn 0 0 100 100 re f", true, false, false, Status::Fail),
            ("0 0 0 0 k 0 0 100 100 re f", false, true, false, Status::Pass),
            ("0 0 0 0 k 0 0 100 100 re f", false, false, true, Status::Pass),
        ] {
            let bytes = crate::pdf::tests::ordered_ink_fixture(second, overprint, 1, second_page, reverse);
            let report = crate::pdf::check(&request(&bytes), &bytes).unwrap();
            let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
            assert_eq!(f.status, expected, "{second}, overprint={overprint}, second_page={second_page}, reverse={reverse}: {f:?}");
            assert_ne!(report.status, Status::Pass);
        }
    }
    #[test]
    fn repeated_paint_allowance_prevents_dense_geometry_pass() {
        for (count, expected) in [(1, Status::Pass), (200, Status::Incomplete)] {
            let extra = "10 10 20 20 re f ".repeat(count - 1);
            let bytes = crate::pdf::tests::fixture(true, "Exact#20ink", &extra, None);
            let mut r = request(&bytes);
            r.acceptance.get_mut("geometry_tolerance_mm").unwrap().value = json!(0.001);
            r.identity.measurement_sha256 = crate::digest(&r.acceptance).unwrap();
            let report = crate::pdf::check(&r, &bytes).unwrap();
            let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
            assert_eq!(f.status, expected, "count={count}: {f:?}");
            assert_ne!(report.status, Status::Pass);
        }
    }
    #[test]
    fn optional_empty_contributions_do_not_hide_unexpected_physical_ink() {
        for (tint, expected) in [("0", Status::Pass), ("1", Status::Fail)] {
            let bytes =
                String::from_utf8(crate::pdf::tests::pdf(true)).unwrap().replace("/Ink cs 1 scn", &format!("/Ink cs {tint} scn")).into_bytes();
            let mut r = request(&bytes);
            r.recipe.required_screens = Some(vec![]);
            let f = r.recipe.foundation.as_mut().unwrap();
            f["treatments"]["arbitrary"]["select"] = json!([[{"field":"coverage_exception","operator":"equals","value":true}]]);
            for plate in f["verification"]["plates"].as_array_mut().unwrap() {
                plate["required"] = json!(false);
            }
            r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
            assert!(r.recipe.contract_issues(r.garment).is_empty());
            let report = crate::pdf::check(&r, &bytes).unwrap();
            let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
            assert_eq!(f.status, expected, "tint={tint}: {f:?}");
            for key in ["file/output/contribution", "file/output/coverage"] {
                assert_eq!(report.findings.iter().find(|f| f.obligation.key == key).unwrap().status, Status::NotApplicable);
            }
        }
    }
    #[test]
    fn optional_empty_plate_does_not_waive_an_unconditional_contribution() {
        for (kind, plate_status, link_status) in [
            ("absent", Status::NotApplicable, Status::Incomplete),
            ("zero", Status::Pass, Status::Incomplete),
            ("positive", Status::Fail, Status::Fail),
        ] {
            let bytes = if kind == "absent" {
                crate::pdf::tests::fixture(true, "Other", "", None)
            } else if kind == "zero" {
                String::from_utf8(crate::pdf::tests::pdf(true)).unwrap().replace("/Ink cs 1 scn", "/Ink cs 0 scn").into_bytes()
            } else {
                crate::pdf::tests::pdf(true)
            };
            let mut r = request(&bytes);
            r.recipe.required_screens = Some(vec![]);
            let f = r.recipe.foundation.as_mut().unwrap();
            f["treatments"]["arbitrary"]["select"] = json!([[{"field":"coverage_exception","operator":"equals","value":true}]]);
            f["verification"]["contributions"]["output"]["emptiness"] = json!("required");
            for plate in f["verification"]["plates"].as_array_mut().unwrap() {
                plate["required"] = json!(false);
            }
            r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
            assert!(r.recipe.contract_issues(r.garment).is_empty());
            let denominator = crate::begin(&r).unwrap().findings.len();
            let report = crate::pdf::check(&r, &bytes).unwrap();
            assert_eq!(report.findings.len(), denominator);
            assert_eq!(report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap().status, plate_status);
            for key in ["file/output/contribution", "file/output/coverage"] {
                let f = report.findings.iter().find(|f| f.obligation.key == key).unwrap();
                assert!(!f.obligation.conditional);
                assert_eq!(f.status, link_status, "{kind} {key}: {f:?}");
                if link_status == Status::Incomplete {
                    assert!(f.reason.contains("proven empty"));
                }
            }
        }
    }
    #[test]
    fn a_printing_merged_plate_cannot_prove_its_empty_required_contribution() {
        let bytes = crate::pdf::tests::pdf(true);
        let mut r = merged(&bytes, true);
        r.recipe.foundation.as_mut().unwrap()["verification"]["contributions"]["second-output"]["emptiness"] = json!("required");
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        assert!(r.recipe.contract_issues(r.garment).is_empty());
        let report = crate::pdf::check(&r, &bytes).unwrap();
        for key in ["plate/dark-output", "file/output/contribution", "file/output/coverage"] {
            assert_eq!(report.findings.iter().find(|f| f.obligation.key == key).unwrap().status, Status::Pass);
        }
        for key in ["file/second-output/contribution", "file/second-output/coverage"] {
            let f = report.findings.iter().find(|f| f.obligation.key == key).unwrap();
            assert_eq!(f.status, Status::Incomplete);
            assert!(f.reason.contains("proven empty"));
        }
    }
    #[test]
    fn coincident_knockout_edge_and_small_remnant_have_measure_level_verdicts() {
        for (extra, adjusted, expected) in [("0 scn 20 10 10 20 re f", true, Status::Pass), ("0 scn 10 10 19.9999 20 re f", false, Status::Fail)] {
            let bytes = crate::pdf::tests::fixture(true, "Exact#20ink", extra, None);
            let mut r = request(&bytes);
            if adjusted {
                let k = 25.4 / 72.0;
                let g = r.geometry.as_mut().unwrap().regions.get_mut("region-1").unwrap();
                g.path_mm =
                    vectorcraft_geom::PathData::from_bezpath(&vectorcraft_geom::Rect::new(10.0 * k, 70.0 * k, 20.0 * k, 90.0 * k).to_path(0.000001));
                r.baseline.as_mut().unwrap().regions[0].geometry_sha256 = crate::digest(g).unwrap();
                r.identity.baseline_sha256 = crate::digest(r.baseline.as_ref().unwrap()).unwrap();
            }
            let report = crate::pdf::check(&r, &bytes).unwrap();
            let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
            assert_eq!(f.status, expected, "{extra}: {f:?}");
        }
    }
    #[test]
    fn expected_and_actual_ink_pages_are_both_examined() {
        let bytes = crate::pdf::tests::ordered_ink_fixture("/Ink cs 1 scn 10 10 20 20 re f", true, 1, true, false);
        let bytes = String::from_utf8(bytes).unwrap().replacen("/Ink cs 1 scn", "/Ink cs 0 scn", 1).into_bytes();
        let report = crate::pdf::check(&request(&bytes), &bytes).unwrap();
        let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
        assert_eq!(f.status, Status::Fail, "{f:?}");
        assert_eq!(f.observed["reconstruction"]["pages_examined"], json!([0, 1]));
        assert_eq!(f.observed["measurements"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn unbalanced_marked_content_never_proves_plate_presence_or_absence() {
        let bytes = crate::pdf::tests::fixture(true, "Exact#20ink", "EMC", None);
        let report = crate::pdf::check(&request(&bytes), &bytes).unwrap();
        let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
        assert_eq!(f.status, Status::Incomplete, "{f:?}");
    }

    #[test]
    fn physical_knockout_follows_content_even_when_ocg_declaration_order_disagrees() {
        for (clear_last, expected) in [(true, Status::Fail), (false, Status::Pass)] {
            let ink = "/OC /Lower BDC /Ink cs 1 scn 10 10 20 20 re f EMC";
            let clear = "/OC /Upper BDC 0 0 0 0 k 0 0 100 100 re f EMC";
            let stream = if clear_last { format!("{ink} {clear}") } else { format!("{clear} {ink}") };
            let objects = vec![
                "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R] /D << /Order [6 0 R 5 0 R] /ON [5 0 R 6 0 R] >> >> >>".into(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /Lower 5 0 R /Upper 6 0 R >> /ColorSpace << /Ink [/Separation /Exact#20ink /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>] >> >> /Contents 4 0 R >>".into(),
                format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
                "<< /Type /OCG /Name (Layer) >>".into(),
                "<< /Type /OCG /Name (Upper) >>".into(),
            ];
            let bytes = crate::pdf::tests::objects_pdf(&objects);
            let report = crate::pdf::check(&request(&bytes), &bytes).unwrap();
            let f = report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap();
            assert_eq!(f.status, expected, "clear_last={clear_last}: {f:?}");
        }
    }
}
