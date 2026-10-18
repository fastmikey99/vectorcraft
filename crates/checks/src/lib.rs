//! Production conformity. Enumerate the denominator before resolving sources or outputs.
//! A finding is never a waiver. Preview state is deliberately absent from this contract.
#![forbid(unsafe_code)]

pub mod attribution;
mod contract;
mod contribution;
pub mod geometry;
#[cfg(not(target_arch = "wasm32"))]
pub mod independent;
mod measure;
pub mod ordering;
mod paint_fidelity;
pub mod pdf;
pub mod preparation;
pub mod prepare;
pub mod prepare_edits;
pub mod program;
#[cfg(not(target_arch = "wasm32"))]
pub mod raster;
#[cfg(not(target_arch = "wasm32"))]
mod raster_images;
pub mod recipe;
mod specification;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const CONTRACT_VERSION: u32 = 1;
pub const ENGINE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "/checks-1/", env!("CHECK_ENGINE_SOURCE_SHA256"));

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Domain-separated semantic encoding shared with Reporter. Every number is finite IEEE-754
/// binary64, so JSON's 1/1.0 and exponent formatting cannot change an input's identity.
pub fn digest<T: Serialize>(value: &T) -> Result<String, String> {
    fn size(h: &mut Sha256, n: usize) {
        h.update((n as u64).to_be_bytes());
    }
    fn encode(h: &mut Sha256, v: &Value) -> Result<(), String> {
        match v {
            Value::Null => h.update(b"n"),
            Value::Bool(b) => h.update(if *b { b"t" } else { b"f" }),
            Value::Number(n) => {
                let n = n.as_f64().filter(|n| n.is_finite()).ok_or("Invalid numeric identity")?;
                h.update(b"d");
                h.update((if n == 0.0 { 0.0 } else { n }).to_be_bytes());
            }
            Value::String(s) => {
                h.update(b"s");
                size(h, s.len());
                h.update(s.as_bytes());
            }
            Value::Array(a) => {
                h.update(b"a");
                size(h, a.len());
                for v in a {
                    encode(h, v)?;
                }
            }
            Value::Object(o) => {
                h.update(b"o");
                size(h, o.len());
                let mut entries: Vec<_> = o.iter().collect();
                entries.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
                for (k, v) in entries {
                    encode(h, &Value::String(k.clone()))?;
                    encode(h, v)?;
                }
            }
        }
        Ok(())
    }
    let mut h = Sha256::new();
    h.update(b"vectorcraft-check-identity-v1\0");
    encode(&mut h, &serde_json::to_value(value).map_err(|e| e.to_string())?)?;
    Ok(format!("{:x}", h.finalize()))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub recipe_sha256: String,
    pub baseline_sha256: String,
    pub original_sha256: String,
    pub document_sha256: String,
    pub pdf_sha256: String,
    pub job_revision: String,
    pub assignments_sha256: String,
    pub measurement_sha256: String,
    pub engine_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub contract_version: u32,
    pub run_id: String,
    pub identity: Identity,
    pub recipe: recipe::Recipe,
    pub garment: recipe::Garment,
    pub input_class: Option<preparation::Fact>,
    pub baseline: Option<preparation::Baseline>,
    /// No manufacturing defaults: unset values remain input obligations.
    pub acceptance: BTreeMap<String, preparation::Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<program::Inputs>,
}
impl Request {
    /// Baseline identity already binds region assignments; include job classification and
    /// garment here so changing either invalidates an otherwise identical PDF/recipe result.
    pub fn assignments_digest(&self) -> Result<String, String> {
        digest(&serde_json::json!({"garment":self.garment,"input_class":self.input_class}))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    Incomplete,
    NotApplicable,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    Operator,
    RecipeOwner,
    Engineering,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Validity {
    Current,
    Stale,
    Provisional,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Obligation {
    pub key: String,
    pub area: String,
    pub label: String,
    pub rule: Option<String>,
    pub file_link: Option<String>,
    pub plate: Option<String>,
    pub owner: Owner,
    pub expected: Value,
    /// Globals and mandatory physical plates cannot be made inapplicable.
    pub conditional: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub artifact_sha256: String,
    pub producer: String,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub page: usize,
    pub bounds_mm: Option<[f64; 4]>,
    pub object_ids: Vec<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub obligation: Obligation,
    pub status: Status,
    pub reason: String,
    pub observed: Value,
    pub evidence: Vec<Evidence>,
    pub location: Option<Location>,
    pub uncertainty_mm: Option<f64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub incomplete: usize,
    pub not_applicable: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub contract_version: u32,
    pub run_id: String,
    pub identity: Identity,
    pub status: Status,
    pub validity: Validity,
    pub coverage: Coverage,
    pub findings: Vec<Finding>,
}

/// Only the trusted verification service issues this receipt, after checking these exact inputs.
/// The receipt's evidence and qualification artifacts must be retained by that service.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReceipt {
    pub run_id: String,
    pub identity: Identity,
    pub interpreter: String,
    pub qualification_sha256: String,
    pub evidence_sha256: String,
}
impl VerificationReceipt {
    pub fn matches(&self, report: &Report) -> bool {
        let hash = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        self.run_id == report.run_id
            && self.identity == report.identity
            && !self.interpreter.is_empty()
            && hash(&self.qualification_sha256)
            && hash(&self.evidence_sha256)
    }
}

/// Keep the entire catalogue in every result, even when an early phase fails.
pub const GLOBALS: &[(&str, &str, &str, Owner)] = &[
    ("recipe", "Recipe and job", "Published recipe identity and complete executable contract", Owner::RecipeOwner),
    ("input_class", "Recipe and job", "Declared individual separation input", Owner::Operator),
    ("preparation", "Preparation", "Approved baseline and complete original source census", Owner::Operator),
    ("assignments", "Preparation", "Every source region and predicate has evidenced assignments", Owner::Operator),
    ("integrity", "PDF integrity", "All printable pages and objects accounted for", Owner::Engineering),
    ("page_specification", "Artwork", "Page, crop, scale, mirror, placement and artwork instances", Owner::Operator),
    ("ink_identity", "Inks", "Exact decoded colourants, routing, tints and definitions", Owner::Engineering),
    ("file_structure", "File construction", "Physical outputs, composite grouping and actual painting order", Owner::Engineering),
    ("colour", "Artwork", "Source paint values, profiles and authorised assignments", Owner::Engineering),
    ("fidelity", "Artwork", "Original-to-baseline and baseline-to-output artwork fidelity", Owner::Engineering),
    ("attribution", "Artwork", "All painted output attributable to authorised content", Owner::Engineering),
    ("compositing", "Printing semantics", "Clipping, masks, transparency, overprint and knockout coverage", Owner::Engineering),
    ("raster", "Raster and gradients", "Prepared masks, tone, source continuity and hard constructed edges", Owner::Engineering),
    ("detail", "Printable detail", "Approved positive/negative feature and local defect requirements", Owner::RecipeOwner),
    ("pdf_metadata", "PDF integrity", "Confirmed print metadata and output intent requirements", Owner::RecipeOwner),
    ("export", "Final PDF", "Exact exported bytes and native-to-output agreement", Owner::Engineering),
    ("independent", "Final PDF", "Qualified independent interpreter and per-ink coverage", Owner::Engineering),
    ("eligibility", "Validity", "Required published version policy and current job identity", Owner::RecipeOwner),
];

pub fn enumerate(request: &Request) -> Result<Vec<Obligation>, String> {
    if request.contract_version != CONTRACT_VERSION {
        return Err("Unsupported check contract version".into());
    }
    if request.identity.engine_version != ENGINE_VERSION {
        return Err("Checker engine version mismatch".into());
    }
    if request.run_id.is_empty() {
        return Err("Check run identity is missing".into());
    }
    let mut obligations: Vec<_> = GLOBALS
        .iter()
        .map(|(key, area, label, owner)| Obligation {
            key: format!("global/{key}"),
            area: (*area).into(),
            label: (*label).into(),
            owner: *owner,
            rule: None,
            file_link: None,
            plate: None,
            expected: Value::Null,
            conditional: false,
        })
        .collect();
    request.recipe.enumerate(request.garment, &mut obligations)?;
    let mut keys = BTreeSet::new();
    if obligations.iter().any(|o| !keys.insert(&o.key)) {
        return Err("Duplicate obligation identity".into());
    }
    Ok(obligations)
}

/// The only starting state is Incomplete. Implemented measurements replace their own slots.
pub fn begin(request: &Request) -> Result<Report, String> {
    let findings = enumerate(request)?
        .into_iter()
        .map(|obligation| Finding {
            obligation,
            status: Status::Incomplete,
            reason: "Measurement has not completed".into(),
            observed: Value::Null,
            evidence: vec![],
            location: None,
            uncertainty_mm: None,
        })
        .collect();
    let mut report = Report {
        contract_version: CONTRACT_VERSION,
        run_id: request.run_id.clone(),
        identity: request.identity.clone(),
        status: Status::Incomplete,
        validity: Validity::Provisional,
        coverage: Coverage::default(),
        findings,
    };
    report.recount();
    Ok(report)
}
impl Report {
    /// Internal measurement sink; service clients cannot supply measurements to the checker.
    pub fn record(&mut self, key: &str, status: Status, reason: impl Into<String>, observed: Value, evidence: Vec<Evidence>) -> Result<(), String> {
        let f = self.findings.iter_mut().find(|f| f.obligation.key == key).ok_or_else(|| format!("Unknown obligation {key}"))?;
        if status != Status::Incomplete && evidence.is_empty() {
            return Err("A verdict requires evidence".into());
        }
        if status == Status::NotApplicable && !f.obligation.conditional {
            return Err("Unconditional obligation cannot be inapplicable".into());
        }
        f.status = status;
        f.reason = reason.into();
        f.observed = observed;
        f.evidence = evidence;
        self.recount();
        Ok(())
    }
    pub fn recount(&mut self) {
        let mut c = Coverage { total: self.findings.len(), ..Coverage::default() };
        for f in &self.findings {
            match f.status {
                Status::Pass => c.passed += 1,
                Status::Fail => c.failed += 1,
                Status::Incomplete => c.incomplete += 1,
                Status::NotApplicable => c.not_applicable += 1,
            }
        }
        self.status = if c.failed > 0 {
            Status::Fail
        } else if c.incomplete > 0 || c.total == c.not_applicable {
            Status::Incomplete
        } else {
            Status::Pass
        };
        self.coverage = c;
    }
    /// History is immutable; eligibility is a separate view against the current identities.
    pub fn validity_for(&self, current: &Identity, receipt: Option<&VerificationReceipt>) -> Validity {
        if &self.identity != current {
            Validity::Stale
        } else if receipt.is_some_and(|r| r.matches(self)) {
            Validity::Current
        } else {
            Validity::Provisional
        }
    }
}

/// Validate a returned result against a locally re-enumerated denominator. Transport never
/// trusts a service's totals, claimed applicability, omitted obligations or stale run identity.
pub fn validate_report(request: &Request, report: &Report) -> Result<(), String> {
    if report.validity != Validity::Provisional {
        return Err("Local checker cannot assert final validity".into());
    }
    if report.contract_version != CONTRACT_VERSION || report.run_id != request.run_id || report.identity != request.identity {
        return Err("Result does not match this request".into());
    }
    let expected = enumerate(request)?;
    let keys: BTreeSet<_> = report.findings.iter().map(|f| &f.obligation.key).collect();
    if keys.len() != report.findings.len() || keys.len() != expected.len() {
        return Err("Incomplete or duplicated result denominator".into());
    }
    for o in expected {
        let f = report.findings.iter().find(|f| f.obligation.key == o.key).ok_or("Missing obligation")?;
        if digest(&o)? != digest(&f.obligation)? {
            return Err("Result changed the obligation".into());
        }
        if f.status != Status::Incomplete && f.evidence.is_empty() {
            return Err("Verdict has no evidence".into());
        }
        if f.status == Status::NotApplicable && !o.conditional {
            return Err("Unconditional obligation was waived".into());
        }
    }
    let mut computed = report.clone();
    computed.recount();
    if computed.status != report.status || digest(&computed.coverage)? != digest(&report.coverage)? {
        return Err("Incorrect coverage or verdict".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    pub(crate) fn request() -> Request {
        serde_json::from_value(json!({
            "contract_version":1,"run_id":"test",
            "identity":{"recipe_sha256":"r","baseline_sha256":"b","original_sha256":"o","document_sha256":"d","pdf_sha256":"p","job_revision":"j","assignments_sha256":"a","measurement_sha256":"m","engine_version":ENGINE_VERSION},
            "recipe":{"id":1,"revision":1,"process":"Renamed","region":"NZ","version":1,"status":"approved","required_screens":["Exact ink"],"profile":"source","evidence":"synthetic",
                "approved_by":"test","approved_at":"2026-10-18","foundation":null,
                "rules":[{"rule_key":"arbitrary","title":"Arbitrary label","garment":"any","scope":"any label","operation":"spread","sources":[{"kind":"artwork","key":"selected"}],"protection":[],"distance_mm":0.2,"join":"round","instruction":"offset","purpose":"test","evidence":"test","confirmation":null}],
                "file_links":[{"key":"output","garment":"any","source_rule":"arbitrary","name_mode":"exact","separation_name":"Exact ink","pdf_layer":"Layer","destination":"Screen","overprint":true,"layer_order":1,"object_group":"","alternate_colour":"","confirmation":null}]},
            "garment":"dark","input_class":null,"baseline":null,"acceptance":{}
        })).unwrap()
    }
    #[test]
    fn empty_sources_and_missing_output_do_not_shrink_denominator() {
        let r = request();
        let report = begin(&r).unwrap();
        assert_eq!(report.coverage.total, GLOBALS.len() + 6 + 6 + 1);
        assert_eq!(report.coverage.incomplete, report.coverage.total);
        assert_eq!(report.status, Status::Incomplete);
        validate_report(&r, &report).unwrap();
    }
    #[test]
    fn omitted_result_duplicate_and_forged_summary_are_rejected() {
        let r = request();
        let report = begin(&r).unwrap();
        let mut omitted = report.clone();
        omitted.findings.pop();
        omitted.recount();
        assert!(validate_report(&r, &omitted).is_err());
        let mut duplicate = report.clone();
        duplicate.findings.push(duplicate.findings[0].clone());
        assert!(validate_report(&r, &duplicate).is_err());
        let mut forged = report.clone();
        forged.status = Status::Pass;
        assert!(validate_report(&r, &forged).is_err());
    }
    #[test]
    fn confirmed_failure_does_not_hide_incomplete_coverage() {
        let mut report = begin(&request()).unwrap();
        report
            .record(
                "legacy-plate/Exact ink",
                Status::Fail,
                "Required plate missing",
                Value::Null,
                vec![Evidence { artifact_sha256: "hash".into(), producer: "test".into(), detail: "test".into() }],
            )
            .unwrap();
        assert_eq!(report.status, Status::Fail);
        assert_eq!(report.coverage.failed, 1);
        assert!(report.coverage.incomplete > 0);
    }
    #[test]
    fn mandatory_plate_and_global_cannot_be_waived() {
        let mut report = begin(&request()).unwrap();
        for key in ["legacy-plate/Exact ink", "global/independent"] {
            assert!(
                report
                    .record(
                        key,
                        Status::NotApplicable,
                        "empty",
                        Value::Null,
                        vec![Evidence { artifact_sha256: "hash".into(), producer: "test".into(), detail: "empty".into() }]
                    )
                    .is_err()
            );
        }
        assert!(report.record("file/output/contribution", Status::NotApplicable, "empty", Value::Null, vec![]).is_err());
    }
    #[test]
    fn every_immutable_identity_invalidates_results() {
        let r = request();
        let report = begin(&r).unwrap();
        let mut value = serde_json::to_value(&r.identity).unwrap();
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        for key in keys {
            let previous = value[&key].clone();
            value[&key] = json!("changed");
            let changed: Identity = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(report.validity_for(&changed, None), Validity::Stale);
            value[&key] = previous;
        }
        assert_eq!(report.validity_for(&r.identity, None), Validity::Provisional);
    }
    #[test]
    fn unknown_operation_still_has_obligations() {
        let mut r = request();
        let before = enumerate(&r).unwrap().len();
        r.recipe.rules[0].operation = "future".into();
        assert_eq!(enumerate(&r).unwrap().len(), before);
        assert!(r.recipe.contract_issues(r.garment).iter().any(|x| x.contains("unknown operation")));
    }
    #[test]
    fn source_absence_requires_evidenced_facts() {
        use preparation::{Applicability, Fact, Provenance, select};
        let sel = json!([[{"field":"role","operator":"equals","value":"silver"}]]);
        assert_eq!(select(&sel, &BTreeMap::new()), Applicability::Unknown);
        let fact = Fact {
            value: json!("ordinary"),
            provenance: Provenance::Operator {
                actor: "test".into(),
                at: "now".into(),
                decision_id: "assignment".into(),
                evidence_sha256: "0".repeat(64),
            },
        };
        let mut facts = BTreeMap::from([("role".into(), fact)]);
        assert_eq!(select(&sel, &facts), Applicability::DoesNotApply);
        facts.get_mut("role").unwrap().value = json!("silver");
        assert_eq!(select(&sel, &facts), Applicability::Applies);
    }
}
