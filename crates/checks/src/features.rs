//! Explicit round morphological feature survival. This is not a universal
//! line-width measurement: corner changes and component/hole survival are part
//! of the specified model and must be approved as such.
use crate::{Status, geometry};
use serde::{Deserialize, Serialize};
use vectorcraft_geom::{FillRule, PathData};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    PositiveOpening,
    NegativeClosing,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub operation: Operation,
    pub diameter_mm: f64,
    pub maximum_boundary_change_mm: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub criterion: Criterion,
    pub candidate_status: Status,
    #[serde(serialize_with = "serialize_measurement")]
    pub measurement: geometry::Measurement,
    pub numerical_construction_allowance_mm: f64,
    /// Offset fit tolerances are not a certified enclosure. The evidence is
    /// retained, but cannot alone establish production conformity.
    pub construction_qualified: bool,
}
fn serialize_measurement<S: serde::Serializer>(m: &geometry::Measurement, serializer: S) -> Result<S::Ok, S::Error> {
    let mut value = serde_json::to_value(m).map_err(serde::ser::Error::custom)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("evaluations");
    }
    value.serialize(serializer)
}
/// Compute the declared opening/closing without dropping small contours. The
/// fit tolerance controls construction work; it is not manufacturing acceptance.
pub fn observe(path: &PathData, criterion: &Criterion, precision_mm: f64) -> Result<Observation, String> {
    observe_with_budget(path, criterion, precision_mm, 0.0, &mut crate::feature_offset::WorkBudget::new())
}
fn observe_with_budget(
    path: &PathData,
    criterion: &Criterion,
    precision_mm: f64,
    input_allowance: f64,
    budget: &mut crate::feature_offset::WorkBudget,
) -> Result<Observation, String> {
    if path.is_empty() {
        return Err("No physical ink coverage on this page; no feature observation was made".into());
    }
    if !input_allowance.is_finite()
        || input_allowance < 0.0
        || !criterion.diameter_mm.is_finite()
        || criterion.diameter_mm <= 0.0
        || criterion.diameter_mm > 1000.0
        || !criterion.maximum_boundary_change_mm.is_finite()
        || criterion.maximum_boundary_change_mm < 0.0
        || !precision_mm.is_finite()
        || !(0.000001..=0.001).contains(&precision_mm)
        || precision_mm * 16.0 >= criterion.diameter_mm
        || path.anchor_count() > 20_000
        || path.subpaths.iter().any(|s| !s.closed || s.anchors.len() < 2)
    {
        return Err("Invalid feature-survival criteria, unresolved open contour or exceeded geometry budget".into());
    }
    geometry::Boundary::new(path, FillRule::NonZero, precision_mm)?;
    let radius = criterion.diameter_mm / 2.0;
    let first = match criterion.operation {
        Operation::PositiveOpening => -radius,
        Operation::NegativeClosing => radius,
    };
    let intermediate = crate::feature_offset::offset_with_budget(path, first, precision_mm, budget)?;
    let result = crate::feature_offset::offset_with_budget(&intermediate.path, -first, precision_mm, budget)?;
    let allowance = input_allowance + intermediate.band_approximation_mm + result.band_approximation_mm;
    let mut measurement = geometry::compare(path, &result.path, criterion.maximum_boundary_change_mm, precision_mm)?;
    // Never turn a numeric construction candidate into a manufacturing verdict.
    // Keep the original candidate result and distances in the observation.
    let same_topology =
        measurement.expected_components == measurement.observed_components && measurement.expected_holes == measurement.observed_holes;
    let candidate = match measurement.status {
        Status::Pass if measurement.upper_mm + allowance > criterion.maximum_boundary_change_mm => Status::Incomplete,
        Status::Fail if same_topology && (measurement.lower_mm - allowance).max(0.0) <= criterion.maximum_boundary_change_mm => Status::Incomplete,
        status => status,
    };
    measurement.reason = format!("Unqualified morphological construction candidate ({candidate:?}): {}", measurement.reason);
    measurement.status = Status::Incomplete;
    Ok(Observation {
        criterion: criterion.clone(),
        candidate_status: candidate,
        measurement,
        numerical_construction_allowance_mm: allowance,
        construction_qualified: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_geom::{Rect, Shape};
    fn rectangle(w: f64, h: f64) -> PathData {
        PathData::from_bezpath(&Rect::new(0.0, 0.0, w, h).to_path(0.000001))
    }
    #[test]
    fn opening_exposes_disappearing_thin_components_without_area_suppression() {
        let criterion = Criterion { operation: Operation::PositiveOpening, diameter_mm: 0.2, maximum_boundary_change_mm: 0.05 };
        for width in [0.05, 0.1, 0.19] {
            let result = observe(&rectangle(width, 1.0), &criterion, 0.0001).unwrap();
            assert_eq!(result.measurement.expected_components, 1);
            assert_eq!(result.measurement.observed_components, 0);
            assert_eq!(result.measurement.status, Status::Incomplete);
            assert!(!result.construction_qualified);
        }
        let result = observe(&rectangle(1.0, 1.0), &criterion, 0.0001).unwrap();
        assert_eq!(result.measurement.observed_components, 1);
        assert!(result.measurement.lower_mm > 0.04, "round opening explicitly changes square corners");
        assert!(result.measurement.upper_mm < 0.05);
    }
    #[test]
    fn closing_reports_a_lost_gap_without_promoting_candidate_to_pass() {
        let a = rectangle(1.0, 1.0);
        let b = rectangle(1.0, 1.0).transformed(vectorcraft_geom::Affine::translate((1.1, 0.0)));
        let path = crate::program::union([a, b]).unwrap();
        let result =
            observe(&path, &Criterion { operation: Operation::NegativeClosing, diameter_mm: 0.2, maximum_boundary_change_mm: 0.05 }, 0.0001).unwrap();
        assert_eq!(result.measurement.expected_components, 2);
        assert_eq!(result.measurement.observed_components, 1);
        assert_eq!(result.measurement.status, Status::Incomplete);
    }
    #[test]
    fn empty_coverage_and_unresolved_allowance_do_not_produce_pass_candidates() {
        let c = Criterion { operation: Operation::PositiveOpening, diameter_mm: 0.2, maximum_boundary_change_mm: 0.05 };
        assert!(observe(&PathData::default(), &c, 0.0001).unwrap_err().contains("No physical ink coverage"));
        let result = observe_with_budget(&rectangle(1.0, 1.0), &c, 0.0001, 0.1, &mut crate::feature_offset::WorkBudget::new()).unwrap();
        assert_eq!(result.candidate_status, Status::Incomplete);
        let value = serde_json::to_value(result).unwrap();
        assert!(value["measurement"].get("evaluations").is_none());
        let mut budget = crate::feature_offset::WorkBudget::new();
        budget.charge(2_000_000).unwrap();
        assert!(observe_with_budget(&rectangle(1.0, 1.0), &c, 0.0001, 0.0, &mut budget).unwrap_err().contains("Aggregate"));
    }
    #[test]
    fn invalid_criteria_and_unresolved_contours_fail_explicitly() {
        let mut c = Criterion { operation: Operation::PositiveOpening, diameter_mm: 0.2, maximum_boundary_change_mm: 0.01 };
        for diameter in [0.0, -1.0, f64::NAN, f64::INFINITY, 1001.0] {
            c.diameter_mm = diameter;
            assert!(observe(&rectangle(1.0, 1.0), &c, 0.0001).is_err());
        }
        c.diameter_mm = 0.2;
        let mut open = rectangle(1.0, 1.0);
        open.subpaths[0].closed = false;
        assert!(observe(&open, &c, 0.0001).is_err());
        assert!(observe(&rectangle(1.0, 1.0), &c, 0.01).is_err());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Specification {
    version: u32,
    model: String,
    units: String,
    job_revision: String,
    baseline_sha256: String,
    recipe_sha256: String,
    plates: Vec<PlateCriteria>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlateCriteria {
    key: String,
    positive_diameter_mm: f64,
    negative_diameter_mm: f64,
    maximum_boundary_change_mm: f64,
}
/// Retain measurements under explicitly reviewed criteria. This path cannot
/// award production conformity until the construction envelope is qualified.
pub(crate) fn check(request: &crate::Request, census: &crate::pdf::Census, report: &mut crate::Report) -> Result<(), String> {
    let Some(fact) = request.acceptance.get("feature_survival") else { return Ok(()) };
    let mut budget = crate::feature_offset::WorkBudget::new();
    let mut measurements = vec![];
    let mut issues = vec![];
    let result = (|| -> Result<(), String> {
        if crate::digest(&request.acceptance)? != request.identity.measurement_sha256
            || !fact.evidenced()
            || !matches!(fact.provenance, crate::preparation::Provenance::Operator { .. })
        {
            return Err("Feature criteria require current acceptance identity and an authenticated approval".into());
        }
        let spec: Specification = serde_json::from_value(fact.value.clone()).map_err(|e| format!("Invalid feature criteria: {e}"))?;
        if spec.version != 1
            || spec.model != "round_open_close_topology"
            || spec.units != "mm"
            || spec.job_revision != request.identity.job_revision
            || spec.baseline_sha256 != request.identity.baseline_sha256
            || spec.recipe_sha256 != request.identity.recipe_sha256
            || spec.plates.is_empty()
            || spec.plates.len() > 64
            || spec.plates.iter().any(|p| p.key.is_empty() || p.key.chars().count() > 256)
        {
            return Err("Feature criteria model, units or immutable job/baseline/recipe binding is invalid".into());
        }
        let active = request
            .recipe
            .foundation
            .as_ref()
            .and_then(|f| f.pointer("/verification/plates"))
            .and_then(serde_json::Value::as_array)
            .ok_or("Complete physical plate contract is required for feature criteria")?;
        let active: Vec<_> =
            active.iter().filter(|p| p.get("garment").and_then(serde_json::Value::as_str).is_some_and(|g| request.garment.matches(g))).collect();
        let keys: std::collections::BTreeSet<_> = spec.plates.iter().map(|p| p.key.as_str()).collect();
        if keys.len() != spec.plates.len()
            || keys.len() != active.len()
            || active.iter().any(|p| p.get("key").and_then(serde_json::Value::as_str).is_none_or(|k| !keys.contains(k)))
            || census.pages == 0
            || census.pages.saturating_mul(spec.plates.len()) > 128
        {
            return Err("Feature criteria must account for every active plate within the measurement budget".into());
        }
        for plate in active {
            let key = plate.get("key").and_then(serde_json::Value::as_str).ok_or("Physical plate key missing")?;
            let criteria = spec.plates.iter().find(|p| p.key == key).ok_or("Physical plate feature criteria missing")?;
            let Some(ink) = plate
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|_| plate.get("name_mode").and_then(serde_json::Value::as_str) == Some("exact"))
            else {
                issues.push(format!("{key}: composite/process feature interpretation requires explicit channel criteria"));
                continue;
            };
            for page in 0..census.pages {
                let coverage = match crate::coverage::solid_with_budget(census, page, ink, &mut budget) {
                    Ok(coverage) => coverage,
                    Err(reason) => {
                        issues.push(format!("{key}, page {page}: {reason}"));
                        continue;
                    }
                };
                if coverage.path.is_empty() {
                    issues.push(format!("{key}, page {page}: no physical ink coverage; no feature observation was made"));
                    continue;
                }
                for (operation, diameter_mm) in
                    [(Operation::PositiveOpening, criteria.positive_diameter_mm), (Operation::NegativeClosing, criteria.negative_diameter_mm)]
                {
                    let criterion = Criterion { operation, diameter_mm, maximum_boundary_change_mm: criteria.maximum_boundary_change_mm };
                    match observe_with_budget(&coverage.path, &criterion, 0.0001, coverage.construction_allowance_mm, &mut budget) {
                        Ok(observation) => measurements.push(serde_json::json!({"plate":key,"ink":ink,"page":page,"coverage_construction_allowance_mm":coverage.construction_allowance_mm,"observation":observation})),
                        Err(reason) => issues.push(format!("{key}, page {page}, {operation:?}: {reason}")),
                    }
                }
            }
        }
        Ok(())
    })();
    if let Err(reason) = result {
        issues.push(reason);
    }
    let mut evidence = fact.evidence();
    evidence.push(crate::Evidence {
        artifact_sha256: request.identity.pdf_sha256.clone(),
        producer: "checks/feature-survival-observations-1".into(),
        detail:
            "Bounded binary physical coverage and explicit round morphological candidate; no area suppression or automatic line-width interpretation"
                .into(),
    });
    report.record(
        "global/detail",
        Status::Incomplete,
        "Feature-survival observations retained; offset/topology error enclosure and production criteria qualification remain required",
        serde_json::json!({"model":"round_open_close_topology","construction_qualified":false,"measurements":measurements,"issues":issues}),
        evidence,
    )
}

#[cfg(test)]
mod request_tests {
    use super::*;
    use crate::{
        digest,
        preparation::{Fact, Provenance},
    };
    use serde_json::json;
    fn fixture() -> (crate::Request, Vec<u8>) {
        let bytes = crate::pdf::tests::pdf(true);
        let mut request = crate::attribution::tests::request();
        request.identity.pdf_sha256 = crate::sha256(&bytes);
        request.acceptance.insert("feature_survival".into(), Fact {
            value: json!({"version":1,"model":"round_open_close_topology","units":"mm","job_revision":request.identity.job_revision,
                "baseline_sha256":request.identity.baseline_sha256,"recipe_sha256":request.identity.recipe_sha256,"plates":[{"key":"dark-output","positive_diameter_mm":0.2,"negative_diameter_mm":0.2,"maximum_boundary_change_mm":0.05}]}),
            provenance: Provenance::Operator { actor:"Synthetic author".into(),at:"2026-10-19".into(),decision_id:"Synthetic criteria only".into(),evidence_sha256:"e".repeat(64) },
        });
        request.identity.measurement_sha256 = digest(&request.acceptance).unwrap();
        (request, bytes)
    }
    fn finding(request: &crate::Request, bytes: &[u8]) -> crate::Finding {
        crate::pdf::check(request, bytes).unwrap().findings.into_iter().find(|f| f.obligation.key == "global/detail").unwrap()
    }
    #[test]
    fn approved_criteria_retain_measurements_but_do_not_qualify_construction() {
        let (request, bytes) = fixture();
        let f = finding(&request, &bytes);
        assert_eq!(f.status, Status::Incomplete);
        assert_eq!(f.observed["measurements"].as_array().unwrap().len(), 2, "{}", f.observed);
        assert_eq!(f.observed["issues"], json!([]));
        assert_eq!(f.observed["construction_qualified"], json!(false));
        assert_eq!(f.evidence[1].artifact_sha256, crate::sha256(&bytes));
    }
    #[test]
    fn stale_missing_duplicated_or_differently_bound_criteria_do_not_measure() {
        for mutation in ["identity", "job", "baseline", "recipe", "plate", "duplicate", "derived", "long-key"] {
            let (mut request, bytes) = fixture();
            let fact = request.acceptance.get_mut("feature_survival").unwrap();
            match mutation {
                "job" => fact.value["job_revision"] = json!("different"),
                "baseline" => fact.value["baseline_sha256"] = json!("f".repeat(64)),
                "recipe" => fact.value["recipe_sha256"] = json!("f".repeat(64)),
                "plate" => fact.value["plates"][0]["key"] = json!("unknown"),
                "long-key" => fact.value["plates"][0]["key"] = json!("x".repeat(257)),
                "duplicate" => {
                    let duplicate = fact.value["plates"][0].clone();
                    fact.value["plates"].as_array_mut().unwrap().push(duplicate);
                }
                "derived" => {
                    fact.provenance =
                        Provenance::Derived { artifact_sha256: "e".repeat(64), algorithm: "unapproved assertion".into(), version: "1".into() }
                }
                _ => {}
            }
            request.identity.measurement_sha256 = if mutation == "identity" { "0".repeat(64) } else { digest(&request.acceptance).unwrap() };
            let f = finding(&request, &bytes);
            assert_eq!(f.status, Status::Incomplete);
            assert_eq!(f.observed["measurements"], json!([]), "{mutation}");
            assert!(!f.observed["issues"].as_array().unwrap().is_empty(), "{mutation}");
        }
    }
}

#[cfg(test)]
mod oracle_fixtures {
    use super::*;
    use vectorcraft_geom::{BezPath, Rect, Shape};
    fn rect(x: f64, y: f64, w: f64, h: f64) -> PathData {
        PathData::from_bezpath(&Rect::new(x, y, x + w, y + h).to_path(0.000001))
    }
    fn hole(mut outer: PathData, points: &[(f64, f64)]) -> PathData {
        let mut b = BezPath::new();
        b.move_to(points[0]);
        for &point in points.iter().skip(1) {
            b.line_to(point);
        }
        b.close_path();
        outer.subpaths.extend(PathData::from_bezpath(&b).subpaths);
        outer
    }
    #[test]
    fn original_feature_matrix_retains_inputs_outputs_and_uncertainty() {
        let mut cases = vec![];
        for (name, w) in [("thin", 0.1), ("below", 0.19), ("boundary", 0.2), ("above", 0.21), ("wide", 1.0)] {
            cases.push((format!("positive-{name}"), rect(0.0, 0.0, w, 1.0), Operation::PositiveOpening));
        }
        for (name, gap) in [("narrow", 0.1), ("below", 0.19), ("boundary", 0.2), ("above", 0.21)] {
            cases.push((
                format!("negative-{name}"),
                crate::program::union([rect(0.0, 0.0, 1.0, 1.0), rect(1.0 + gap, 0.0, 1.0, 1.0)]).unwrap(),
                Operation::NegativeClosing,
            ));
        }
        cases.push((
            "positive-tiny-island".into(),
            crate::program::union([rect(0.0, 0.0, 1.0, 1.0), rect(3.0, 0.0, 0.001, 0.001)]).unwrap(),
            Operation::PositiveOpening,
        ));
        cases.push((
            "negative-tiny-hole".into(),
            hole(rect(0.0, 0.0, 1.0, 1.0), &[(0.45, 0.45), (0.45, 0.55), (0.55, 0.55), (0.55, 0.45)]),
            Operation::NegativeClosing,
        ));
        cases.push((
            "negative-acute-counter".into(),
            hole(rect(0.0, 0.0, 12.0, 12.0), &[(2.0, 2.0), (6.0, 10.0), (10.0, 2.0)]),
            Operation::NegativeClosing,
        ));
        let circle = |r| PathData::from_bezpath(&vectorcraft_geom::kurbo::Circle::new((0.0, 0.0), r).to_path(0.000001));
        cases.push(("positive-circle".into(), circle(0.3), Operation::PositiveOpening));
        let mut ring = circle(1.0);
        let mut inner = circle(0.9);
        inner.reverse();
        ring.subpaths.extend(inner.subpaths);
        cases.push(("positive-thin-ring".into(), ring, Operation::PositiveOpening));
        let mut counter = circle(1.0);
        let mut inner = circle(0.09);
        inner.reverse();
        counter.subpaths.extend(inner.subpaths);
        cases.push(("negative-round-counter".into(), counter, Operation::NegativeClosing));
        let separated = PathData::new(
            (0..16).flat_map(|i| circle(0.3).transformed(vectorcraft_geom::Affine::translate((f64::from(i) * 3.0, 0.0))).subpaths).collect(),
        );
        cases.push(("positive-clustered-circles".into(), separated, Operation::PositiveOpening));
        let mut evidence = vec![];
        for (name, input, operation) in cases {
            let criterion = Criterion { operation, diameter_mm: 0.2, maximum_boundary_change_mm: 0.05 };
            let sign = if matches!(operation, Operation::PositiveOpening) { -1.0 } else { 1.0 };
            let result = crate::feature_offset::offset(&input, sign * 0.1, 0.0001)
                .and_then(|first| crate::feature_offset::offset(&first.path, -sign * 0.1, 0.0001));
            let observation = observe(&input, &criterion, 0.0001);
            if let Ok(observation) = &observation {
                assert_eq!(observation.measurement.status, Status::Incomplete);
                assert!(!observation.construction_qualified);
            }
            evidence.push(serde_json::json!({"name":name,"criterion":criterion,"input":input,
                "result":result.as_ref().ok().map(|r|&r.path),"construction_error":result.as_ref().err(),
                "observation":observation.as_ref().ok(),"measurement_error":observation.as_ref().err()}));
        }
        if let Some(root) = std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR") {
            let root = std::path::PathBuf::from(root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("feature-matrix.json"),
                serde_json::to_vec_pretty(&serde_json::json!({"engine":crate::ENGINE_VERSION,"cases":evidence})).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(evidence.len(), 16);
    }
}
