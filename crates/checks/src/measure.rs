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
/// A PDF layer is a file-structure obligation, not a physical ink boundary.
/// Include process paint anywhere in the PDF. A named ink can be excluded only
/// when a different declared exact plate accounts for it. Ambiguous routing is
/// unresolved; never manufacture a physical Pass from a layer-local subset.
fn physical_paints<'a>(
    request: &Request,
    plate: &Value,
    plates: &[Value],
    pieces: &[&program::Piece],
    census: &'a Census,
) -> Result<Vec<&'a crate::pdf::PaintedObject>, String> {
    let mode = plate.get("name_mode").and_then(Value::as_str);
    let ink = plate.get("name").and_then(Value::as_str);
    if mode == Some("exact") {
        return Ok(census.paints.iter().filter(|p| p.ink.as_deref() == ink).collect());
    }
    let active: Vec<_> = plates.iter().filter(|p| p.get("garment").and_then(Value::as_str).is_some_and(|g| request.garment.matches(g))).collect();
    if active.iter().filter(|p| p.get("name_mode").and_then(Value::as_str) != Some("exact")).count() != 1 {
        return Err("Physical process/source routing across multiple destinations requires independent separation evidence".into());
    }
    let mut objects = vec![];
    for paint in &census.paints {
        if paint.process || pieces.iter().any(|p| p.ink.is_some() && p.ink == paint.ink) {
            objects.push(paint);
        } else if let Some(name) = paint.ink.as_deref() {
            if !active
                .iter()
                .any(|p| p.get("name_mode").and_then(Value::as_str) == Some("exact") && p.get("name").and_then(Value::as_str) == Some(name))
            {
                return Err(format!("Named ink {name:?} has no proven physical output route"));
            }
        } else {
            return Err("Observed paint has no proven physical output route".into());
        }
    }
    Ok(objects)
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
    for plate in plates.iter().filter(|p| p.get("garment").and_then(Value::as_str).is_some_and(|g| request.garment.matches(g))) {
        let key = plate.get("key").and_then(Value::as_str).ok_or("Plate key missing")?;
        let links: Vec<_> = plate.get("links").and_then(Value::as_array).ok_or("Plate links missing")?.iter().filter_map(Value::as_str).collect();
        let ready = links.iter().all(|k| program.outputs.contains_key(*k));
        let pieces: Vec<_> = links.iter().flat_map(|k| program.outputs.get(*k).into_iter().flatten()).collect();
        let ink = plate.get("name").and_then(Value::as_str);
        let objects = match physical_paints(request, plate, plates, &pieces, census) {
            Ok(objects) => objects,
            Err(reason) => {
                report.record(&format!("plate/{key}"), Status::Incomplete, reason, Value::Null, evidence(request))?;
                all_geometry = false;
                continue;
            }
        };
        let printable = objects.iter().any(|p| p.geometry_mm.as_ref().is_some_and(|g| !g.is_empty()));
        let required = plate.get("required").and_then(Value::as_bool) == Some(true);
        let complete = census.unresolved.is_empty() && ready && objects.iter().all(|p| p.geometry_mm.is_some() && p.page.is_some());
        if !complete {
            all_geometry = false;
            continue;
        }
        if required && !printable {
            report.record(
                &format!("plate/{key}"),
                Status::Fail,
                "Unconditionally required physical plate is empty",
                json!({"ink":ink}),
                evidence(request),
            )?;
            any_failure = true;
            all_geometry = false;
            continue;
        }
        if pieces.iter().all(|p| p.path.is_empty()) && !printable && !required {
            report.record(
                &format!("plate/{key}"),
                Status::NotApplicable,
                "All declared contributions proven empty from the original census",
                json!({"links":links}),
                evidence(request),
            )?;
            continue;
        }
        if tolerance.is_none() || !accepted_identity {
            report.record(
                &format!("plate/{key}"),
                Status::Incomplete,
                "Approved geometric acceptance is required for measured conformity",
                json!({"objects":objects.len()}),
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
        let pages: BTreeSet<_> = pieces.iter().map(|p| p.page).chain(objects.iter().filter_map(|p| p.page)).collect();
        for page in pages {
            let expected = program::union(pieces.iter().filter(|p| p.page == page).map(|p| p.path.clone()))?;
            let observed = program::union(objects.iter().filter(|p| p.page == Some(page)).filter_map(|p| p.geometry_mm.clone()))?;
            let construction_error = pieces.iter().filter(|p| p.page == page).map(|p| p.construction_error_mm).fold(0.0, f64::max)
                + objects.iter().filter(|p| p.page == Some(page)).map(|p| p.geometry_error_mm).fold(0.0, f64::max);
            let precision = (tolerance / 16.0).clamp(0.000001, 0.001);
            match geometry::compare(&expected, &observed, tolerance, precision) {
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
                            object_ids: objects.iter().filter(|p| p.page == Some(page)).take(64).map(|p| p.id).collect(),
                        });
                    }
                    uncertainty = uncertainty.max(m.uncertainty_mm);
                    measurements.push(json!({"page":page,"measurement":m}));
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
                "Combined physical coverage compared against the complete recipe graph",
                json!({"acceptance_mm":tolerance,"measurements":measurements}),
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
            "Physical coverage measured; actual painting order and independent print semantics still require reconciliation"
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
    fn physical_process_coverage_includes_unexpected_layers_and_keeps_exact_inks_separate() {
        let bytes = crate::pdf::tests::pdf(true);
        let r = request(&bytes);
        let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        let exact = json!({"key":"technical","garment":"dark","name_mode":"exact","name":"Exact ink"});
        let composite = json!({"key":"art","garment":"dark","name_mode":"composite"});
        let source = json!({"key":"source","garment":"dark","name_mode":"source"});
        let mut process = census.paints[0].clone();
        process.ink = None;
        process.process = true;
        process.layer = "Unexpected OCG".into();
        census.paints.push(process);
        let plates = [exact.clone(), composite.clone()];
        let observed = physical_paints(&r, &composite, &plates, &[], &census).unwrap();
        assert_eq!(observed.len(), 2);
        assert!(observed.iter().any(|p| p.layer == "Unexpected OCG"));
        assert!(observed.iter().all(|p| p.process));
        let exact_observed = physical_paints(&r, &exact, &plates, &[], &census).unwrap();
        assert_eq!(exact_observed.len(), 1);
        assert_eq!(exact_observed[0].ink.as_deref(), Some("Exact ink"));
        // No declared route for the technical ink: do not discard it and pass.
        assert!(physical_paints(&r, &composite, std::slice::from_ref(&composite), &[], &census).is_err());
        // Process ink cannot be assigned to two destinations from OCG names.
        assert!(physical_paints(&r, &composite, &[exact.clone(), composite.clone(), source.clone()], &[], &census).is_err());
        assert_eq!(physical_paints(&r, &source, &[exact, source.clone()], &[], &census).unwrap().len(), 2);
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
}
