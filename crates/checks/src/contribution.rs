//! Attribute measured physical coverage without inventing object-to-rule lineage.
use crate::{Evidence, Finding, Report, Request, Status, program::Program};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn combine(statuses: impl IntoIterator<Item = Status>) -> Status {
    let mut seen = false;
    let mut incomplete = false;
    for status in statuses {
        seen = true;
        if status == Status::Fail {
            return Status::Fail;
        }
        incomplete |= status != Status::Pass;
    }
    if seen && !incomplete { Status::Pass } else { Status::Incomplete }
}
fn direct(plate: &Finding) -> bool {
    plate.obligation.expected.get("links").and_then(Value::as_array).is_some_and(|links| links.len() == 1)
}
fn topology(plate: &Finding) -> Status {
    let Some(measurements) = plate.observed.get("measurements").and_then(Value::as_array).filter(|m| !m.is_empty()) else {
        return Status::Incomplete;
    };
    combine(measurements.iter().map(|m| {
        let Some(m) = m.get("measurement") else { return Status::Incomplete };
        let counts = ["expected_components", "observed_components", "expected_holes", "observed_holes"].map(|key| m.get(key).and_then(Value::as_u64));
        match counts {
            [Some(a), Some(b), Some(c), Some(d)] if a == b && c == d => Status::Pass,
            [Some(_), Some(_), Some(_), Some(_)] => Status::Fail,
            _ => Status::Incomplete,
        }
    }))
}
pub fn check(request: &Request, program: &Program, report: &mut Report) -> Result<(), String> {
    let plates: Vec<_> = report.findings.iter().filter(|f| f.obligation.key.starts_with("plate/")).cloned().collect();
    let links: Vec<_> = request.recipe.file_links.iter().filter(|l| request.garment.matches(&l.garment)).collect();
    let evidence = vec![
        Evidence {
            artifact_sha256: request.identity.recipe_sha256.clone(),
            producer: "checks/contribution-1".into(),
            detail: "All declared same-plate contributions are constructed before comparison; no object grouping or construction history is assumed"
                .into(),
        },
        Evidence {
            artifact_sha256: request.identity.pdf_sha256.clone(),
            producer: "checks/pdf-boundary-1".into(),
            detail: "References complete physical-coverage measurements, including numerical bounds and topology".into(),
        },
        Evidence {
            artifact_sha256: request.identity.baseline_sha256.clone(),
            producer: "checks/prepared-geometry-1".into(),
            detail: "Each contribution retains approved source regions and deterministic construction".into(),
        },
    ];
    let references = |keys: &BTreeSet<&str>| -> Vec<&Finding> {
        plates
            .iter()
            .filter(|p| {
                p.obligation
                    .expected
                    .get("links")
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.iter().any(|l| l.as_str().is_some_and(|l| keys.contains(l))))
            })
            .collect()
    };
    for link in &links {
        let key = format!("file/{}/contribution", link.key);
        if report.findings.iter().any(|f| f.obligation.key == key && f.status == Status::NotApplicable) {
            continue;
        }
        let empty_required = report.findings.iter().any(|f| f.obligation.key == key && !f.obligation.conditional)
            && program.outputs.get(&link.key).is_some_and(|pieces| pieces.iter().all(|p| p.path.is_empty()));
        if empty_required {
            let coverage = format!("file/{}/coverage", link.key);
            // A physical plate can pass with no ink, or with ink from another
            // contribution. Neither proves a source-empty unconditional link.
            // Keep independently established failures instead of weakening them.
            if report.findings.iter().any(|f| f.obligation.key == coverage && f.status != Status::Fail) {
                report.record(
                    &coverage,
                    Status::Incomplete,
                    "Unconditional contribution has proven empty expected source; physical plate conformity cannot establish its coverage",
                    json!({"source_rule":link.source_rule,"expected_source_empty":true}),
                    evidence.clone(),
                )?;
            }
        }
        let measured = references(&BTreeSet::from([link.key.as_str()]));
        let status = if program.outputs.contains_key(&link.key) {
            combine(measured.iter().map(|p| if p.status == Status::Pass || direct(p) { p.status } else { Status::Incomplete }))
        } else {
            Status::Incomplete
        };
        let status = if empty_required && status == Status::Pass { Status::Incomplete } else { status };
        report.record(
            &key,
            status,
            if empty_required && status == Status::Incomplete {
                "Unconditional contribution has proven empty expected source; physical plate conformity cannot establish its contribution"
            } else {
                "Contribution evaluated through the complete physical output; a failing merged output does not identify which contribution is wrong"
            },
            json!({"physical_findings":measured.iter().map(|f| &f.obligation.key).collect::<Vec<_>>(),"source_rule":link.source_rule}),
            evidence.clone(),
        )?;
    }
    for rule in request.recipe.rules.iter().filter(|r| r.operation != "policy" && request.garment.matches(&r.garment)) {
        let geometry_key = format!("rule/{}/geometry", rule.rule_key);
        if report.findings.iter().any(|f| f.obligation.key == geometry_key && f.status == Status::NotApplicable) {
            continue;
        }
        let routed: Vec<_> = links.iter().filter(|l| l.source_rule == rule.rule_key).collect();
        let keys = routed.iter().map(|l| l.key.as_str()).collect();
        let measured = references(&keys);
        // Output-ink routing may select only part of a rule. A passing selected plate
        // cannot establish the geometry of pieces that were never mapped to an output.
        let complete = program.rules.get(&rule.rule_key).is_some_and(|pieces| {
            !routed.is_empty()
                && pieces.iter().all(|piece| {
                    routed.iter().any(|l| {
                        program
                            .outputs
                            .get(&l.key)
                            .is_some_and(|out| out.iter().any(|p| p.page == piece.page && p.path == piece.path && p.regions == piece.regions))
                    })
                })
        });
        let geometry = if complete {
            combine(measured.iter().map(|p| if p.status == Status::Pass || direct(p) { p.status } else { Status::Incomplete }))
        } else {
            Status::Incomplete
        };
        let topology = if complete {
            combine(measured.iter().map(|p| {
                if p.status == Status::Pass {
                    Status::Pass
                } else if direct(p) {
                    topology(p)
                } else {
                    Status::Incomplete
                }
            }))
        } else {
            Status::Incomplete
        };
        let observed = json!({"physical_findings":measured.iter().map(|f| &f.obligation.key).collect::<Vec<_>>(),"complete_rule_routing":complete,"unresolved_construction":program.unresolved.get(&rule.rule_key)});
        for (facet, status) in [("geometry", geometry), ("topology", topology)] {
            let reason = if facet == "topology"
                && status == Status::Incomplete
                && measured.iter().any(|p| p.reason == "Unconditionally required physical plate is empty after ordered ink compositing")
            {
                "Physical plate absence is established; topology was not measured"
            } else {
                "Rule assessed through all mapped physical outputs; shared boundaries covered by other authorised contributions need no separate objects"
            };
            report.record(&format!("rule/{}/{facet}", rule.rule_key), status, reason, observed.clone(), evidence.clone())?;
            if let Some(f) = report.findings.iter_mut().find(|f| f.obligation.key == format!("rule/{}/{facet}", rule.rule_key)) {
                f.location = measured.iter().find_map(|p| p.location.clone());
                f.uncertainty_mm = measured.iter().filter_map(|p| p.uncertainty_mm).reduce(f64::max);
            }
        }
        // A failed shape alone does not establish a protection violation. A passing
        // complete output does prove the constructed exclusion together with all
        // authorised overlapping contributions.
        let protection = if rule.protection.is_empty() {
            Status::NotApplicable
        } else if geometry == Status::Pass {
            Status::Pass
        } else {
            Status::Incomplete
        };
        report.record(
            &format!("rule/{}/protection", rule.rule_key),
            protection,
            if rule.protection.is_empty() {
                "Pinned rule declares no protected sources"
            } else {
                "Protected-source exclusions evaluated as part of the complete combined output"
            },
            observed,
            evidence.clone(),
        )?;
    }
    Ok(())
}
