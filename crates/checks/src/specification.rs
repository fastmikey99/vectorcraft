//! Explicit, immutable job and print-metadata requirements. No defaults are inferred
//! from the PDF being checked. Measurement criteria live in provenance-bearing facts.
use crate::{Evidence, Report, Request, Status, digest};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageSpecification {
    version: u32,
    units: String,
    job_revision: String,
    baseline_sha256: String,
    tolerance_mm: f64,
    pages: Vec<Page>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    media_mm: [f64; 4],
    crop_mm: [f64; 4],
    bleed_mm: [f64; 4],
    trim_mm: [f64; 4],
    art_mm: [f64; 4],
    rotation: u16,
    user_unit: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataSpecification {
    version: u32,
    job_revision: String,
    baseline_sha256: String,
    #[serde(deserialize_with = "Option::deserialize")]
    catalog_intents: Option<Vec<Intent>>,
    page_intents: Vec<Option<Vec<Intent>>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    subtype: String,
    #[serde(deserialize_with = "Option::deserialize")]
    profile_sha256: Option<String>,
}
impl Intent {
    fn valid(&self) -> bool {
        !self.subtype.is_empty()
            && self.subtype.len() <= 256
            && self.profile_sha256.as_ref().is_none_or(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    }
    fn matches(&self, observed: &vectorcraft_pdf::ProductionIntent) -> bool {
        self.subtype == observed.subtype && self.profile_sha256 == observed.profile_sha256
    }
}
fn fact<T: serde::de::DeserializeOwned>(request: &Request, key: &str) -> Result<T, String> {
    if digest(&request.acceptance)? != request.identity.measurement_sha256 {
        return Err("Measurement/job acceptance identity mismatch".into());
    }
    let fact = request.acceptance.get(key).filter(|f| f.evidenced()).ok_or_else(|| format!("Approved {key} requirement is missing"))?;
    if !matches!(fact.provenance, crate::preparation::Provenance::Operator { .. }) {
        return Err(format!("Approved {key} requires an operator decision"));
    }
    serde_json::from_value(fact.value.clone()).map_err(|e| format!("Invalid {key}: {e}"))
}
fn evidence(request: &Request, key: &str) -> Vec<Evidence> {
    let mut evidence = request.acceptance.get(key).map(|f| f.evidence()).unwrap_or_default();
    evidence.push(Evidence {
        artifact_sha256: request.identity.pdf_sha256.clone(),
        producer: "checks/production-specification-1".into(),
        detail: "Fresh raw PDF page dictionaries and bounded output profiles; no editor or XMP assertions".into(),
    });
    evidence
}
fn pages(request: &Request, info: &vectorcraft_pdf::ProductionInfo, report: &mut Report) -> Result<(), String> {
    let spec: PageSpecification = fact(request, "page_specification")?;
    if spec.version != 1
        || spec.units != "mm"
        || spec.job_revision != request.identity.job_revision
        || spec.baseline_sha256 != request.identity.baseline_sha256
        || spec.pages.is_empty()
        || spec.pages.len() > 1000
        || !spec.tolerance_mm.is_finite()
        || spec.tolerance_mm < 0.0
        || spec.pages.iter().any(|p| {
            ![0, 90, 180, 270].contains(&p.rotation)
                || !p.user_unit.is_finite()
                || p.user_unit <= 0.0
                || p.user_unit > 75000.0
                || [p.media_mm, p.crop_mm, p.bleed_mm, p.trim_mm, p.art_mm]
                    .iter()
                    .any(|b| b.iter().any(|n| !n.is_finite() || n.abs() > 1.0e12) || b[0] >= b[2] || b[1] >= b[3])
        })
    {
        return Err("Invalid or differently bound page specification".into());
    }
    let mut failures = vec![];
    let mut unresolved = vec![];
    if spec.pages.len() != info.pages.len() {
        failures.push(json!({"field":"page_count","expected":spec.pages.len(),"observed":info.pages.len()}));
    }
    for (page, (want, got)) in spec.pages.iter().zip(&info.pages).enumerate() {
        if want.rotation != got.rotation || want.user_unit != got.user_unit {
            failures.push(json!({"page":page,"field":"rotation_or_user_unit","expected":[f64::from(want.rotation),want.user_unit],"observed":[f64::from(got.rotation),got.user_unit]}));
        }
        for (name, expected, observed) in [
            ("media", want.media_mm, got.media_mm),
            ("crop", want.crop_mm, got.crop_mm),
            ("bleed", want.bleed_mm, got.bleed_mm),
            ("trim", want.trim_mm, got.trim_mm),
            ("art", want.art_mm, got.art_mm),
        ] {
            for (coordinate, (&a, &b)) in expected.iter().zip(&observed).enumerate() {
                let error = (a - b).abs();
                // IEEE-754 conversion uncertainty, not a manufacturing tolerance.
                let uncertainty = 16.0 * f64::EPSILON * (a.abs().max(b.abs()).max(1.0));
                let row = json!({"page":page,"box":name,"coordinate":coordinate,"expected_mm":a,"observed_mm":b,"error_mm":error,"uncertainty_mm":uncertainty});
                if error - uncertainty > spec.tolerance_mm {
                    failures.push(row);
                } else if error + uncertainty > spec.tolerance_mm {
                    unresolved.push(row);
                }
            }
        }
    }
    let matched = failures.is_empty() && unresolved.is_empty();
    report.record(
        "global/page_specification",
        if failures.is_empty() { Status::Incomplete } else { Status::Fail },
        if !unresolved.is_empty() {
            "Page-box acceptance overlaps numerical uncertainty; zero tolerance cannot establish physical agreement"
        } else {
            "Page boxes, physical units and rotation compared; artwork placement requires all physical output geometry and original replay"
        },
        json!({"page_dictionaries_match":matched,"tolerance_mm":spec.tolerance_mm,"failures":failures,"unresolved":unresolved,"pages":info.pages}),
        evidence(request, "page_specification"),
    )?;
    finalize_placement(report)
}
/// Called again after the worker has freshly replayed preparation from original bytes.
pub(crate) fn finalize_placement(report: &mut Report) -> Result<(), String> {
    let original = report.findings.iter().any(|f| f.obligation.key == "global/fidelity" && f.observed.get("original_verified") == Some(&json!(true)));
    let prepared = report.findings.iter().any(|f| f.obligation.key == "global/preparation" && f.status == Status::Pass);
    let plates: Vec<_> =
        report.findings.iter().filter(|f| f.obligation.key.starts_with("plate/") || f.obligation.key.starts_with("legacy-plate/")).collect();
    let complete = !plates.is_empty() && plates.iter().all(|f| matches!(f.status, Status::Pass | Status::NotApplicable));
    let dependencies: Vec<_> = report
        .findings
        .iter()
        .filter(|f| {
            f.obligation.key == "global/fidelity"
                || f.obligation.key == "global/preparation"
                || f.obligation.key.starts_with("plate/")
                || f.obligation.key.starts_with("legacy-plate/")
        })
        .flat_map(|f| f.evidence.clone())
        .collect();
    if let Some(f) = report.findings.iter_mut().find(|f| {
        f.obligation.key == "global/page_specification" && f.observed.get("page_dictionaries_match") == Some(&json!(true)) && f.status != Status::Fail
    }) {
        f.status = if original && prepared && complete { Status::Pass } else { Status::Incomplete };
        f.reason = if f.status == Status::Pass {
            "Approved page dictionaries and all physical output geometry match the freshly replayed prepared artwork without alignment"
        } else {
            "Page dictionaries match; placement still requires current original replay, approved preparation and every physical output"
        }
        .into();
        f.observed["placement_dependencies_verified"] = json!(f.status == Status::Pass);
        if f.status == Status::Pass {
            for item in dependencies {
                if !f.evidence.iter().any(|e| e.artifact_sha256 == item.artifact_sha256 && e.producer == item.producer && e.detail == item.detail) {
                    f.evidence.push(item);
                }
            }
        }
    }
    report.recount();
    Ok(())
}

fn metadata(request: &Request, info: &vectorcraft_pdf::ProductionInfo, report: &mut Report) -> Result<(), String> {
    let spec: MetadataSpecification = fact(request, "pdf_metadata")?;
    if spec.version != 1
        || spec.job_revision != request.identity.job_revision
        || spec.baseline_sha256 != request.identity.baseline_sha256
        || spec.page_intents.len() > 1000
        || spec.catalog_intents.as_ref().map_or(0, Vec::len) + spec.page_intents.iter().flatten().map(Vec::len).sum::<usize>() > 64
        || spec.catalog_intents.iter().flatten().chain(spec.page_intents.iter().flatten().flatten()).any(|i| !i.valid())
    {
        return Err("Invalid print metadata requirement".into());
    }
    // Null requires absence. An empty list requires an explicitly present empty array.
    let equal = |want: &Option<Vec<Intent>>, got: &Option<Vec<vectorcraft_pdf::ProductionIntent>>| match (want, got) {
        (None, None) => true,
        (Some(want), Some(got)) => want.len() == got.len() && want.iter().zip(got).all(|(a, b)| a.matches(b)),
        _ => false,
    };
    let matches = equal(&spec.catalog_intents, &info.intents)
        && spec.page_intents.len() == info.pages.len()
        && spec.page_intents.iter().zip(&info.pages).all(|(a, b)| equal(a, &b.intents));
    report.record(
        "global/pdf_metadata",
        if matches { Status::Pass } else { Status::Fail },
        "Exact catalog/page output intent subtypes and embedded ICC byte identities compared to the approved requirement",
        json!({"catalog_intents":info.intents,"page_intents":info.pages.iter().map(|p| &p.intents).collect::<Vec<_>>()}),
        evidence(request, "pdf_metadata"),
    )
}
pub(crate) fn check(request: &Request, bytes: &[u8], report: &mut Report) -> Result<(), String> {
    if !request.acceptance.contains_key("page_specification") && !request.acceptance.contains_key("pdf_metadata") {
        return Ok(());
    }
    let info = vectorcraft_pdf::production_info(bytes);
    for (key, result) in [
        ("global/page_specification", info.as_ref().map_err(|e| e.to_string()).and_then(|i| pages(request, i, report))),
        ("global/pdf_metadata", info.as_ref().map_err(|e| e.to_string()).and_then(|i| metadata(request, i, report))),
    ] {
        if let Err(reason) = result {
            report.record(key, Status::Incomplete, reason, Value::Null, vec![])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preparation::{Fact, Provenance};
    fn set(request: &mut Request, key: &str, value: Value) {
        request.acceptance.insert(
            key.into(),
            Fact {
                value,
                provenance: Provenance::Operator {
                    actor: "Synthetic fixture author".into(),
                    at: "2026-10-19".into(),
                    decision_id: "Test criteria only".into(),
                    evidence_sha256: "e".repeat(64),
                },
            },
        );
        request.identity.measurement_sha256 = digest(&request.acceptance).unwrap();
    }
    fn request() -> (Request, Vec<u8>) {
        let bytes = crate::pdf::tests::pdf(true);
        let mut r = crate::attribution::tests::request();
        r.identity.pdf_sha256 = crate::sha256(&bytes);
        let edge = 100.0 * 25.4 / 72.0;
        let b = json!([0.0, 0.0, edge, edge]);
        let spec = json!({"version":1,"units":"mm","job_revision":r.identity.job_revision,"baseline_sha256":r.identity.baseline_sha256,
            "tolerance_mm":0.01,"pages":[{"media_mm":b,"crop_mm":b,"bleed_mm":b,"trim_mm":b,"art_mm":b,"rotation":0,"user_unit":1.0}]});
        set(&mut r, "page_specification", spec);
        let metadata = json!({"version":1,"job_revision":r.identity.job_revision,"baseline_sha256":r.identity.baseline_sha256,"catalog_intents":null,"page_intents":[null]});
        set(&mut r, "pdf_metadata", metadata);
        (r, bytes)
    }
    fn finding(r: &Request, bytes: &[u8], key: &str) -> crate::Finding {
        let mut report = crate::begin(r).unwrap();
        check(r, bytes, &mut report).unwrap();
        report.findings.into_iter().find(|f| f.obligation.key == key).unwrap()
    }
    #[test]
    fn page_limits_use_absolute_coordinates_and_preserve_uncertainty() {
        for (shift, status, matched) in
            [(0.0, Status::Incomplete, true), (0.009, Status::Incomplete, true), (0.01, Status::Incomplete, false), (0.011, Status::Fail, false)]
        {
            let (mut r, bytes) = request();
            let mut spec = r.acceptance["page_specification"].value.clone();
            spec["pages"][0]["media_mm"][0] = json!(shift);
            set(&mut r, "page_specification", spec);
            let f = finding(&r, &bytes, "global/page_specification");
            assert_eq!(f.status, status, "{shift}: {}", f.observed);
            assert_eq!(f.observed["page_dictionaries_match"], json!(matched));
        }
    }
    #[test]
    fn metadata_requires_explicit_provenance_and_exact_full_inventory() {
        let (mut r, bytes) = request();
        assert_eq!(finding(&r, &bytes, "global/pdf_metadata").status, Status::Pass);
        let metadata = json!({"version":1,"job_revision":r.identity.job_revision,"baseline_sha256":r.identity.baseline_sha256,"catalog_intents":[{"subtype":"GTS_PDFX","profile_sha256":null}],"page_intents":[null]});
        set(&mut r, "pdf_metadata", metadata);
        assert_eq!(finding(&r, &bytes, "global/pdf_metadata").status, Status::Fail);
        r.identity.measurement_sha256 = "f".repeat(64);
        assert_eq!(finding(&r, &bytes, "global/pdf_metadata").status, Status::Incomplete);
        r.acceptance.clear();
        assert_eq!(finding(&r, &bytes, "global/pdf_metadata").status, Status::Incomplete);
    }
    #[test]
    fn metadata_job_binding_and_zero_tolerance_are_explicit() {
        let (mut r, bytes) = request();
        let mut spec = r.acceptance["pdf_metadata"].value.clone();
        spec["job_revision"] = json!("other-job");
        set(&mut r, "pdf_metadata", spec);
        assert_eq!(finding(&r, &bytes, "global/pdf_metadata").status, Status::Incomplete);
        let mut spec = r.acceptance["page_specification"].value.clone();
        spec["tolerance_mm"] = json!(0.0);
        set(&mut r, "page_specification", spec);
        let result = finding(&r, &bytes, "global/page_specification");
        assert_eq!(result.status, Status::Incomplete);
        assert!(result.reason.contains("zero tolerance"));
    }
    #[test]
    fn derived_or_file_assertions_cannot_approve_job_criteria() {
        for key in ["page_specification", "pdf_metadata"] {
            let (mut r, bytes) = request();
            r.acceptance.get_mut(key).unwrap().provenance =
                Provenance::Derived { artifact_sha256: "a".repeat(64), algorithm: "upstream-claim".into(), version: "1".into() };
            r.identity.measurement_sha256 = digest(&r.acceptance).unwrap();
            assert_eq!(finding(&r, &bytes, &format!("global/{key}")).status, Status::Incomplete);
        }
    }
    #[test]
    fn placement_requires_replayed_source_and_every_physical_output() {
        let (r, bytes) = request();
        let mut report = crate::begin(&r).unwrap();
        check(&r, &bytes, &mut report).unwrap();
        for f in &mut report.findings {
            if f.obligation.key == "global/preparation" || f.obligation.key.starts_with("plate/") {
                f.status = Status::Pass;
            }
        }
        finalize_placement(&mut report).unwrap();
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "global/page_specification").unwrap().status, Status::Incomplete);
        report.findings.iter_mut().find(|f| f.obligation.key == "global/fidelity").unwrap().observed = json!({"original_verified":true});
        finalize_placement(&mut report).unwrap();
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "global/page_specification").unwrap().status, Status::Pass);
        report.findings.iter_mut().find(|f| f.obligation.key == "global/fidelity").unwrap().observed = json!({"original_verified":false});
        finalize_placement(&mut report).unwrap();
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "global/page_specification").unwrap().status, Status::Incomplete);
    }
    #[test]
    fn wrong_job_rotation_count_and_unrecognised_fields_cannot_pass() {
        for (field, value, status) in [
            ("job_revision", json!("another"), Status::Incomplete),
            ("version", json!(2), Status::Incomplete),
            ("pages", json!([]), Status::Incomplete),
            ("invented", json!(true), Status::Incomplete),
        ] {
            let (mut r, bytes) = request();
            let mut spec = r.acceptance["page_specification"].value.clone();
            spec[field] = value;
            set(&mut r, "page_specification", spec);
            assert_eq!(finding(&r, &bytes, "global/page_specification").status, status);
        }
        let (mut r, bytes) = request();
        let mut spec = r.acceptance["page_specification"].value.clone();
        spec["pages"][0]["rotation"] = json!(90);
        set(&mut r, "page_specification", spec);
        assert_eq!(finding(&r, &bytes, "global/page_specification").status, Status::Fail);
    }
}
