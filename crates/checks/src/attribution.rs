//! Resolve source applicability from the approved original census, never from missing output.
//! This is a selection/dependency plan. Geometry and output paint are measured separately.
use crate::{
    Evidence, Report, Request, Status, digest,
    preparation::{Applicability, Fact, select},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Selection {
    pub regions: BTreeSet<String>,
    pub unresolved: BTreeSet<String>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Plan {
    pub rules: BTreeMap<String, Selection>,
    /// Expected facts after explicit assignment policies. Original facts remain in the baseline.
    pub facts: BTreeMap<String, BTreeMap<String, Fact>>,
    pub paints: BTreeMap<String, Value>,
    pub issues: BTreeSet<String>,
}
fn evaluate(selection: &Value, facts: &BTreeMap<String, BTreeMap<String, Fact>>, candidates: &BTreeSet<String>) -> Selection {
    let mut out = Selection::default();
    for id in candidates {
        match facts.get(id).map(|f| select(selection, f)).unwrap_or(Applicability::Unknown) {
            Applicability::Applies => {
                out.regions.insert(id.clone());
            }
            Applicability::Unknown => {
                out.unresolved.insert(format!("{id}: selection facts unresolved"));
            }
            Applicability::DoesNotApply => {}
        }
    }
    out
}
pub fn resolve(request: &Request) -> Result<Plan, String> {
    let baseline = request.baseline.as_ref().ok_or("Approved preparation baseline required")?;
    if !baseline.issues().is_empty()
        || digest(baseline)? != request.identity.baseline_sha256
        || baseline.original_sha256 != request.identity.original_sha256
        || baseline.job_revision != request.identity.job_revision
    {
        return Err("Preparation census/identity is not valid for this run".into());
    }
    let f = request.recipe.foundation.as_ref().ok_or("Executable foundation required")?;
    if !crate::contract::issues(&request.recipe).is_empty() || digest(&request.recipe)? != request.identity.recipe_sha256 {
        return Err("Executable recipe contract/identity is invalid".into());
    }
    let mut plan = Plan { facts: baseline.regions.iter().map(|r| (r.id.clone(), r.facts.clone())).collect(), ..Default::default() };
    let all: BTreeSet<_> = plan.facts.keys().cloned().collect();
    let active: Vec<_> = request.recipe.rules.iter().filter(|r| request.garment.matches(&r.garment)).collect();
    let mut assignments: BTreeMap<(String, String), Value> = BTreeMap::new();
    for r in active.iter().filter(|r| r.operation == "policy") {
        let policy = f.get("policies").and_then(|p| p.get(&r.rule_key)).ok_or("Missing policy")?;
        let kind = policy.get("kind").and_then(Value::as_str);
        if kind == Some("reference") {
            continue;
        }
        let selected = evaluate(policy.get("when").unwrap_or(&Value::Null), &plan.facts, &all);
        plan.issues.extend(selected.unresolved.iter().cloned());
        if kind == Some("operator") && !selected.regions.is_empty() {
            plan.issues
                .insert(format!("{}: requires a concrete authorised construction alternative; acknowledgement is not a measurement", r.rule_key));
        }
        if kind == Some("assignment") {
            for id in &selected.regions {
                for (field, value) in [("role", policy.get("role")), ("paint", policy.get("paint"))] {
                    let Some(value) =
                        value.filter(|v| !v.is_null() && !(field == "paint" && v.get("mode").and_then(Value::as_str) == Some("source")))
                    else {
                        continue;
                    };
                    let key = (id.clone(), field.to_string());
                    if assignments.get(&key).is_some_and(|previous| previous != value) {
                        plan.issues.insert(format!("{id}: conflicting {field} assignment policies"));
                        continue;
                    }
                    assignments.insert(key, value.clone());
                    if field == "paint" {
                        plan.paints.insert(id.clone(), value.clone());
                    } else if let Some(facts) = plan.facts.get_mut(id) {
                        facts.insert(
                            field.into(),
                            Fact {
                                value: value.clone(),
                                provenance: crate::preparation::Provenance::Derived {
                                    artifact_sha256: request.identity.recipe_sha256.clone(),
                                    algorithm: format!("recipe-policy/{}", r.rule_key),
                                    version: request.recipe.revision.to_string(),
                                },
                            },
                        );
                    }
                }
            }
        }
        plan.rules.insert(r.rule_key.clone(), selected);
    }
    let mut pending: Vec<_> = active.into_iter().filter(|r| r.operation != "policy").collect();
    while !pending.is_empty() {
        let before = pending.len();
        pending.retain(|r| {
            if r.sources.iter().any(|s| s.kind == "rule" && !plan.rules.contains_key(&s.key)) {
                return true;
            }
            let mut source = Selection::default();
            for s in &r.sources {
                let selected = if s.kind == "artwork" {
                    evaluate(f.get("selections").and_then(|v| v.get(&s.key)).unwrap_or(&Value::Null), &plan.facts, &all)
                } else {
                    plan.rules.get(&s.key).cloned().unwrap_or_default()
                };
                source.regions.extend(selected.regions);
                source.unresolved.extend(selected.unresolved);
            }
            let treatment = f.get("treatments").and_then(|t| t.get(&r.rule_key)).and_then(|t| t.get("select")).unwrap_or(&Value::Null);
            let mut selected = evaluate(treatment, &plan.facts, &source.regions);
            selected.unresolved.extend(source.unresolved);
            plan.issues.extend(selected.unresolved.iter().cloned());
            plan.rules.insert(r.rule_key.clone(), selected);
            false
        });
        if pending.len() == before {
            return Err("Unresolved/cyclic source graph".into());
        }
    }
    for (id, facts) in &plan.facts {
        let routes = f.get("input_routes").and_then(Value::as_array).ok_or("Input routes missing")?;
        let mut matches = 0;
        let mut unknown = false;
        for route in routes {
            match select(route.get("when").unwrap_or(&Value::Null), facts) {
                Applicability::Applies => matches += 1,
                Applicability::Unknown => unknown = true,
                Applicability::DoesNotApply => {}
            }
        }
        if unknown || matches != 1 {
            plan.issues.insert(format!("{id}: needs exactly one evidenced input route ({matches} matching)"));
        }
    }
    Ok(plan)
}
pub fn check(request: &Request, report: &mut Report) -> Result<Option<Plan>, String> {
    let plan = match resolve(request) {
        Ok(p) => p,
        Err(reason) => {
            let dependent: Vec<_> = report
                .findings
                .iter()
                .filter(|f| {
                    f.status == Status::Incomplete
                        && (f.obligation.key.starts_with("rule/")
                            || f.obligation.key.ends_with("/contribution")
                            || f.obligation.key.ends_with("/coverage")
                            || f.obligation.key.starts_with("plate/"))
                })
                .map(|f| f.obligation.key.clone())
                .collect();
            for key in dependent {
                report.record(
                    &key,
                    Status::Incomplete,
                    format!("Source-dependent conformity is unresolved: {reason}. Final-output observations are reported separately."),
                    Value::Null,
                    vec![],
                )?;
            }
            report.record("global/assignments", Status::Incomplete, reason, Value::Null, vec![])?;
            return Ok(None);
        }
    };
    if request.assignments_digest()? != request.identity.assignments_sha256 {
        return Ok(None);
    }
    let evidence = vec![
        Evidence {
            artifact_sha256: request.identity.baseline_sha256.clone(),
            producer: "checks/source-selection-1".into(),
            detail: "Approved original census and evidenced facts; no finished-output presence test".into(),
        },
        Evidence {
            artifact_sha256: request.identity.recipe_sha256.clone(),
            producer: "checks/recipe-graph-1".into(),
            detail: "Pinned executable selection/dependency graph".into(),
        },
    ];
    report.record(
        "global/assignments",
        if plan.issues.is_empty() { Status::Pass } else { Status::Incomplete },
        "Source routing and assignment plan",
        json!(plan.issues),
        evidence.clone(),
    )?;
    for r in request.recipe.rules.iter().filter(|r| request.garment.matches(&r.garment)) {
        if r.operation == "policy" {
            let p = request.recipe.foundation.as_ref().and_then(|f| f.get("policies")).and_then(|p| p.get(&r.rule_key));
            if p.and_then(|p| p.get("kind")).and_then(Value::as_str) == Some("reference") {
                report.record(
                    &format!("rule/{}/policy", r.rule_key),
                    Status::Pass,
                    "Reference accounted for by linked executable obligations",
                    p.cloned().unwrap_or(Value::Null),
                    evidence.clone(),
                )?;
            } else if let Some(selection) = plan.rules.get(&r.rule_key)
                && selection.unresolved.is_empty()
                && selection.regions.is_empty()
            {
                report.record(
                    &format!("rule/{}/policy", r.rule_key),
                    Status::NotApplicable,
                    "Policy condition proven false for every original region",
                    json!(selection),
                    evidence.clone(),
                )?;
            }
            // Active assignment policies still need output paint verification; operator
            // acknowledgements never satisfy a construction requirement by themselves.
            continue;
        }
        let Some(selection) = plan.rules.get(&r.rule_key) else {
            continue;
        };
        if !selection.unresolved.is_empty() {
            continue;
        }
        if selection.regions.is_empty() {
            for facet in ["applicability", "sources", "geometry", "paint", "topology", "protection"] {
                report.record(
                    &format!("rule/{}/{facet}", r.rule_key),
                    Status::NotApplicable,
                    "Complete original census proves no applicable source",
                    json!(selection),
                    evidence.clone(),
                )?;
            }
        } else {
            for facet in ["applicability", "sources"] {
                report.record(
                    &format!("rule/{}/{facet}", r.rule_key),
                    Status::Pass,
                    "Sources resolved from original preparation facts",
                    json!(selection),
                    evidence.clone(),
                )?;
            }
        }
    }
    for link in request.recipe.file_links.iter().filter(|l| request.garment.matches(&l.garment)) {
        if let Some(selected) = plan.rules.get(&link.source_rule) {
            // Ink-partitioned outputs require the exact matching source ink before proving absence.
            let matching = request
                .recipe
                .foundation
                .as_ref()
                .and_then(|f| f.get("outputs"))
                .and_then(|o| o.get(&link.key))
                .and_then(|o| o.get("select"))
                .and_then(Value::as_str)
                == Some("matching_output_ink");
            let mut selected = selected.clone();
            if matching {
                selected.regions.retain(|id| match plan.facts.get(id).and_then(|f| f.get("ink")).filter(|f| f.evidenced()) {
                    Some(f) => f.value.as_str() == link.separation_name.as_deref(),
                    None => {
                        selected.unresolved.insert(format!("{id}: exact source ink unresolved"));
                        false
                    }
                });
            }
            if selected.regions.is_empty() && selected.unresolved.is_empty() {
                for facet in ["contribution", "identity", "layer", "order", "overprint", "coverage"] {
                    let key = format!("file/{}/{facet}", link.key);
                    if report.findings.iter().any(|f| f.obligation.key == key && f.obligation.conditional) {
                        report.record(
                            &key,
                            Status::NotApplicable,
                            "Declared conditional contribution has proven empty source",
                            json!(selected),
                            evidence.clone(),
                        )?;
                    }
                }
            }
        }
    }
    Ok(Some(plan))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::preparation::{Baseline, Provenance, Region};
    fn fact(value: Value) -> Fact {
        Fact {
            value,
            provenance: Provenance::Operator {
                actor: "fixture author".into(),
                at: "2026-10-18".into(),
                decision_id: "independent fixture".into(),
                evidence_sha256: "a".repeat(64),
            },
        }
    }
    pub(crate) fn request() -> Request {
        let mut r = crate::tests::request();
        r.recipe = crate::contract::tests::recipe();
        let original = "b".repeat(64);
        let b = Baseline {
            id: "prepared".into(),
            revision: 1,
            original_sha256: original.clone(),
            native_sha256: "c".repeat(64),
            job_revision: r.identity.job_revision.clone(),
            selected_pages: vec![0],
            source_census: vec!["original-path-1".into()],
            census_evidence: Evidence {
                artifact_sha256: original.clone(),
                producer: "fixture-census".into(),
                detail: "one independently specified path".into(),
            },
            regions: vec![Region {
                id: "region-1".into(),
                source_objects: vec!["original-path-1".into()],
                geometry_sha256: "d".repeat(64),
                facts: BTreeMap::from([
                    ("role".into(), fact(json!("ordinary"))),
                    ("ink".into(), fact(json!("Exact ink"))),
                    ("coverage_exception".into(), fact(json!(false))),
                ]),
                prepared_mask_sha256: None,
            }],
            intentional_edits: vec![],
            approval: Some(fact(json!("approved"))),
        };
        r.identity.original_sha256 = original;
        r.identity.baseline_sha256 = digest(&b).unwrap();
        r.baseline = Some(b);
        r.identity.recipe_sha256 = digest(&r.recipe).unwrap();
        r.identity.assignments_sha256 = r.assignments_digest().unwrap();
        r.identity.measurement_sha256 = digest(&r.acceptance).unwrap();
        r
    }
    fn refresh(r: &mut Request) {
        r.identity.recipe_sha256 = digest(&r.recipe).unwrap();
        r.identity.baseline_sha256 = digest(r.baseline.as_ref().unwrap()).unwrap();
    }
    #[test]
    fn source_absence_can_waive_a_contribution_but_not_a_required_physical_plate() {
        let mut r = request();
        r.recipe.foundation.as_mut().unwrap()["selections"]["selected"] = json!([[{"field":"role","operator":"equals","value":"silver"}]]);
        refresh(&mut r);
        let mut report = crate::begin(&r).unwrap();
        check(&r, &mut report).unwrap();
        for key in ["rule/arbitrary/geometry", "file/output/coverage"] {
            assert_eq!(report.findings.iter().find(|f| f.obligation.key == key).unwrap().status, Status::NotApplicable);
        }
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "plate/dark-output").unwrap().status, Status::Incomplete);
        assert_eq!(report.coverage.total, crate::enumerate(&r).unwrap().len());
    }
    #[test]
    fn unknown_facts_and_wrong_baseline_identity_never_prove_emptiness() {
        let mut r = request();
        r.recipe.foundation.as_mut().unwrap()["selections"]["selected"] = json!([[{"field":"black","operator":"equals","value":true}]]);
        refresh(&mut r);
        let mut report = crate::begin(&r).unwrap();
        check(&r, &mut report).unwrap();
        assert_eq!(report.coverage.not_applicable, 0);
        r.identity.baseline_sha256 = "wrong".into();
        assert!(resolve(&r).is_err());
    }
    #[test]
    fn ordinary_jobs_do_not_require_an_inactive_stronger_white_alternative() {
        let mut r = request();
        let mut policy = r.recipe.rules[0].clone();
        policy.rule_key = "optional-choice".into();
        policy.operation = "policy".into();
        policy.sources.clear();
        policy.distance_mm = None;
        r.recipe.rules.push(policy);
        r.recipe.foundation.as_mut().unwrap()["policies"]["optional-choice"] =
            json!({"kind":"operator","when":[[{"field":"coverage_exception","operator":"equals","value":true}]],"decision":"stronger-white"});
        refresh(&mut r);
        assert!(r.recipe.contract_issues(r.garment).is_empty());
        let mut report = crate::begin(&r).unwrap();
        let plan = check(&r, &mut report).unwrap().unwrap();
        assert!(plan.issues.is_empty());
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "rule/optional-choice/policy").unwrap().status, Status::NotApplicable);
        r.baseline.as_mut().unwrap().regions[0].facts.insert("coverage_exception".into(), fact(json!(true)));
        refresh(&mut r);
        let mut report = crate::begin(&r).unwrap();
        assert!(!check(&r, &mut report).unwrap().unwrap().issues.is_empty());
        assert_ne!(report.status, Status::Pass);
    }
}
