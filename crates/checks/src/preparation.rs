//! Preparation facts are claims with provenance, not evidence inferred from missing output.
use crate::{Evidence, Report, Request, Status, digest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub value: Value,
    pub provenance: Provenance,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provenance {
    File { artifact_sha256: String, locator: String },
    Derived { artifact_sha256: String, algorithm: String, version: String },
    Operator { actor: String, at: String, decision_id: String, evidence_sha256: String },
}
impl Fact {
    pub fn evidence(&self) -> Vec<Evidence> {
        let (hash, producer, detail) = match &self.provenance {
            Provenance::File { artifact_sha256, locator } => (artifact_sha256.clone(), "preparation/file".into(), locator.clone()),
            Provenance::Derived { artifact_sha256, algorithm, version } => {
                (artifact_sha256.clone(), format!("{algorithm}/{version}"), "Derived preparation fact".into())
            }
            Provenance::Operator { actor, at, decision_id, evidence_sha256 } => {
                (evidence_sha256.clone(), format!("operator/{actor}"), format!("{decision_id} at {at}"))
            }
        };
        vec![Evidence { artifact_sha256: hash, producer, detail }]
    }
    pub fn evidenced(&self) -> bool {
        let hash = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
        match &self.provenance {
            Provenance::File { artifact_sha256, locator } => hash(artifact_sha256) && !locator.is_empty(),
            Provenance::Derived { artifact_sha256, algorithm, version } => hash(artifact_sha256) && !algorithm.is_empty() && !version.is_empty(),
            Provenance::Operator { actor, at, decision_id, evidence_sha256 } => {
                !actor.is_empty() && !at.is_empty() && !decision_id.is_empty() && hash(evidence_sha256)
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub id: String,
    pub source_objects: Vec<String>,
    pub geometry_sha256: String,
    pub facts: BTreeMap<String, Fact>,
    pub prepared_mask_sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub id: String,
    pub revision: u64,
    pub original_sha256: String,
    pub native_sha256: String,
    pub job_revision: String,
    pub selected_pages: Vec<usize>,
    /// Produced by content decoding before assignments, never by the finished separation.
    pub source_census: Vec<String>,
    pub census_evidence: Evidence,
    pub regions: Vec<Region>,
    pub intentional_edits: Vec<Fact>,
    pub approval: Option<Fact>,
}
impl Baseline {
    pub fn issues(&self) -> Vec<String> {
        let mut issues = vec![];
        let census: BTreeSet<_> = self.source_census.iter().collect();
        if census.is_empty() || census.len() != self.source_census.len() {
            issues.push("Original source census is empty or duplicated".into());
        }
        if self.census_evidence.artifact_sha256 != self.original_sha256 {
            issues.push("Census is not bound to the original".into());
        }
        let mut accounted = BTreeSet::new();
        let mut region_ids = BTreeSet::new();
        for r in &self.regions {
            if !region_ids.insert(&r.id) || r.source_objects.is_empty() {
                issues.push(format!("{}: invalid region identity/source", r.id));
            }
            for source in &r.source_objects {
                if !census.contains(source) {
                    issues.push(format!("{}: source outside census", r.id));
                }
                accounted.insert(source);
            }
            if r.facts.is_empty() || r.facts.values().any(|f| !f.evidenced()) {
                issues.push(format!("{}: missing assignment provenance", r.id));
            }
        }
        if accounted != census {
            issues.push("Original printable content is unaccounted for".into());
        }
        if self.approval.as_ref().is_none_or(|f| !f.evidenced() || f.value != json!("approved")) {
            issues.push("Preparation approval missing".into());
        }
        if self.intentional_edits.iter().any(|f| !f.evidenced()) {
            issues.push("Intentional preparation edit lacks evidence".into());
        }
        issues
    }
}

/// Evaluate AND/OR predicates with three-valued logic. Missing facts cannot prove absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applicability {
    Applies,
    DoesNotApply,
    Unknown,
}
pub fn select(selection: &Value, facts: &BTreeMap<String, Fact>) -> Applicability {
    if !crate::contract::selection_valid(selection) {
        return Applicability::Unknown;
    }
    let Some(clauses) = selection.as_array() else {
        return Applicability::Unknown;
    };
    if clauses.is_empty() {
        return Applicability::Unknown;
    }
    let mut unknown = false;
    for clause in clauses {
        let Some(predicates) = clause.as_array() else {
            return Applicability::Unknown;
        };
        let mut clause_unknown = false;
        let mut false_clause = false;
        for p in predicates {
            let Some(field) = p.get("field").and_then(Value::as_str) else {
                return Applicability::Unknown;
            };
            let Some(fact) = facts.get(field).filter(|f| f.evidenced()) else {
                clause_unknown = true;
                continue;
            };
            let Some(value) = p.get("value") else {
                return Applicability::Unknown;
            };
            let matches = match p.get("operator").and_then(Value::as_str) {
                Some("equals") => fact.value == *value,
                Some("not_equals") => fact.value != *value,
                _ => return Applicability::Unknown,
            };
            false_clause |= !matches;
        }
        if !false_clause && !clause_unknown {
            return Applicability::Applies;
        }
        unknown |= !false_clause && clause_unknown;
    }
    if unknown { Applicability::Unknown } else { Applicability::DoesNotApply }
}

pub fn check_inputs(request: &Request, report: &mut Report) -> Result<(), String> {
    if request.assignments_digest()? != request.identity.assignments_sha256 {
        report.record("global/assignments", Status::Incomplete, "Garment/input-class assignment identity mismatch", Value::Null, vec![])?;
    }
    if digest(&request.acceptance)? != request.identity.measurement_sha256 {
        report.record("global/detail", Status::Incomplete, "Measurement acceptance identity mismatch", Value::Null, vec![])?;
    }
    let evidence = vec![Evidence {
        artifact_sha256: digest(&request.recipe)?,
        producer: "checks/recipe-contract-1".into(),
        detail: "Immutable request snapshot".into(),
    }];
    let mut issues = request.recipe.contract_issues(request.garment);
    if digest(&request.recipe)? != request.identity.recipe_sha256 {
        issues.push("Recipe snapshot digest mismatch".into());
    }
    report.record(
        "global/recipe",
        if issues.is_empty() { Status::Pass } else { Status::Incomplete },
        if issues.is_empty() { "Published contract validated".into() } else { issues.join("; ") },
        json!(issues),
        evidence.clone(),
    )?;
    if let Some(class) = request.input_class.as_ref().filter(|c| c.evidenced()) {
        report.record(
            "global/input_class",
            if class.value == json!("individual_separation") { Status::Pass } else { Status::Incomplete },
            "Input class is an explicit job/preparation fact",
            class.value.clone(),
            class.evidence(),
        )?;
    }
    if let Some(b) = &request.baseline {
        let mut issues = b.issues();
        if digest(b)? != request.identity.baseline_sha256
            || b.original_sha256 != request.identity.original_sha256
            || b.job_revision != request.identity.job_revision
        {
            issues.push("Baseline identity mismatch".into());
        }
        // Structural completeness is necessary but cannot certify source-to-baseline fidelity.
        report.record(
            "global/preparation",
            if issues.is_empty() { Status::Pass } else { Status::Incomplete },
            if issues.is_empty() { "Approved census and preparation record validated".into() } else { issues.join("; ") },
            json!(issues),
            vec![b.census_evidence.clone()],
        )?;
    }
    Ok(())
}
