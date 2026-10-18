//! Structural contract validation shared by all execution paths. Never executes recipe prose.
use crate::recipe::{Recipe, Source};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Foundation {
    version: u32,
    verification: Option<Verification>,
    selections: BTreeMap<String, Value>,
    input_routes: Vec<Route>,
    treatments: BTreeMap<String, Treatment>,
    policies: BTreeMap<String, Policy>,
    outputs: BTreeMap<String, Output>,
    colour: Colour,
    geometry: Geometry,
    file: File,
    checks: Checks,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    when: Value,
    destination: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Treatment {
    select: Value,
    edges: String,
    neighbours: Vec<Source>,
    protection: String,
    paint: Paint,
    partition: String,
    result: String,
}
#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum Paint {
    Source,
    OutputInk,
    Cmyk { value: [f64; 4] },
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Policy {
    Assignment { when: Value, role: Option<String>, paint: Paint },
    Operator { when: Value, decision: String },
    Reference { covers: Vec<String> },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    select: String,
    emit: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Colour {
    rgb: String,
    cmyk: String,
    spots: String,
    untagged: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Geometry {
    units: String,
    edges: String,
    raster_outline: String,
    paint_extension: String,
    overlap: String,
    coordinates: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    before: Vec<Order>,
    preserve: Vec<String>,
    editable_groups: String,
    indigo: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Order {
    lower: String,
    upper: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Checks {
    geometry_tolerance_mm: Option<f64>,
    empty_required_output: String,
    unexpected_output: String,
    unknown_assignment: String,
    stale_result: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Verification {
    version: u32,
    plates: Vec<Plate>,
    contributions: BTreeMap<String, Contribution>,
    acceptance: BTreeMap<String, Acceptance>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plate {
    key: String,
    garment: String,
    name: Option<String>,
    name_mode: String,
    destination: String,
    required: bool,
    links: Vec<String>,
    evidence: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Contribution {
    emptiness: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Acceptance {
    value: f64,
    units: String,
    approved_by: String,
    approved_at: String,
    evidence: String,
}
fn valid_key(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 600
}
fn paint_valid(p: &Paint) -> bool {
    match p {
        Paint::Cmyk { value } => value.iter().all(|n| n.is_finite() && (0.0..=100.0).contains(n)),
        _ => true,
    }
}
/// Schema validation precedes applicability. An unknown predicate may not hide behind a false clause.
pub(crate) fn selection_valid(v: &Value) -> bool {
    let Some(clauses) = v.as_array().filter(|a| !a.is_empty() && a.len() <= 12) else {
        return false;
    };
    clauses.iter().all(|c| {
        c.as_array().is_some_and(|ps| {
            ps.len() <= 12
                && ps.iter().all(|p| {
                    let Some(o) = p.as_object().filter(|o| o.len() == 3) else {
                        return false;
                    };
                    let Some(field) = o.get("field").and_then(Value::as_str) else {
                        return false;
                    };
                    let op = o.get("operator").and_then(Value::as_str);
                    if !matches!(op, Some("equals" | "not_equals")) {
                        return false;
                    }
                    let Some(value) = o.get("value") else {
                        return false;
                    };
                    match field {
                        "role" | "ink" | "treatment" | "paint" => value.as_str().is_some_and(valid_key),
                        "cmyk_total" => value.as_f64().is_some_and(f64::is_finite),
                        "black" | "black_requested" | "coverage_exception" => value.is_boolean(),
                        _ => false,
                    }
                })
        })
    })
}
fn verification_issues(v: &Verification, r: &Recipe) -> Vec<String> {
    let mut errors = vec![];
    if v.version != 1 || v.plates.len() > 200 {
        errors.push("Unqualified physical output contract version or size".into());
    }
    let mut keys = BTreeSet::new();
    let mut groups = BTreeSet::new();
    let mut derived = BTreeSet::new();
    for p in &v.plates {
        if !valid_key(&p.key) || !keys.insert(&p.key) {
            errors.push(format!("{}: invalid/duplicate physical output key", p.key));
        }
        if !["dark", "light"].contains(&p.garment.as_str())
            || !["Indigo", "Screen"].contains(&p.destination.as_str())
            || !["exact", "source", "composite"].contains(&p.name_mode.as_str())
            || (p.name_mode == "exact") != p.name.is_some()
            || p.name.as_ref().is_some_and(|n| !valid_key(n))
            || (p.required && p.name_mode != "exact")
            || (p.destination == "Indigo") != (p.name_mode == "composite")
        {
            errors.push(format!("{}: invalid physical output identity", p.key));
        }
        if !groups.insert((&p.garment, &p.destination, &p.name_mode, &p.name)) {
            errors.push(format!("{}: duplicate physical output", p.key));
        }
        if p.required
            && p.destination == "Screen"
            && let Some(n) = &p.name
        {
            derived.insert(n);
        }
        if p.evidence.trim().is_empty() || p.links.is_empty() || p.links.len() > 100 || p.links.iter().collect::<BTreeSet<_>>().len() != p.links.len()
        {
            errors.push(format!("{}: physical output needs unique contributions and evidence", p.key));
        }
        for key in &p.links {
            if !r.file_links.iter().any(|l| {
                &l.key == key
                    && (l.garment == "any" || l.garment == p.garment)
                    && l.destination == p.destination
                    && (p.name_mode == "composite" || (l.name_mode == p.name_mode && l.separation_name == p.name))
            }) {
                errors.push(format!("{}: incompatible contribution {key}", p.key));
            }
        }
    }
    for l in &r.file_links {
        if !v.contributions.get(&l.key).is_some_and(|c| ["required", "conditional_on_proven_source"].contains(&c.emptiness.as_str())) {
            errors.push(format!("{}: contribution requiredness missing or unknown", l.key));
        }
        for garment in ["dark", "light"] {
            if (l.garment == "any" || l.garment == garment)
                && v.plates.iter().filter(|p| p.garment == garment && p.links.contains(&l.key)).count() != 1
            {
                errors.push(format!("{}: requires exactly one {garment} physical output", l.key));
            }
        }
    }
    for key in v.contributions.keys() {
        if !r.file_links.iter().any(|l| &l.key == key) {
            errors.push(format!("Unknown contribution {key}"));
        }
    }
    let declared: BTreeSet<_> = r.required_screens.iter().flatten().collect();
    if r.required_screens.is_none() || declared != derived {
        errors.push("Required-screen compatibility summary disagrees with physical output contract".into());
    }
    for (key, a) in &v.acceptance {
        if !valid_key(key)
            || !a.value.is_finite()
            || a.value < 0.0
            || !["mm", "fraction"].contains(&a.units.as_str())
            || (key.ends_with("_mm") && a.units != "mm")
            || (key.ends_with("_fraction") && a.units != "fraction")
            || (a.units == "fraction" && a.value > 1.0)
            || [&a.approved_by, &a.approved_at, &a.evidence].iter().any(|s| s.trim().is_empty())
        {
            errors.push(format!("{key}: invalid acceptance/provenance"));
        }
    }
    errors
}
pub(crate) fn physical_contract_valid(r: &Recipe) -> bool {
    r.foundation
        .as_ref()
        .and_then(|f| f.get("verification"))
        .and_then(|v| serde_json::from_value::<Verification>(v.clone()).ok())
        .is_some_and(|v| verification_issues(&v, r).is_empty())
}
pub(crate) fn issues(r: &Recipe) -> Vec<String> {
    let mut errors = vec![];
    if r.rules.is_empty() || r.file_links.is_empty() {
        errors.push("Recipe requires construction rules and file outputs".into());
    }
    let Some(raw) = &r.foundation else {
        return errors;
    };
    let f: Foundation = match serde_json::from_value(raw.clone()) {
        Ok(f) => f,
        Err(e) => return vec![format!("Invalid executable contract: {e}")],
    };
    let mut add = |s: String| errors.push(s);
    if f.version != 1 {
        add("Unknown foundation version".into());
    }
    if f.input_routes.is_empty() || f.input_routes.len() > 30 {
        add("One to thirty input routes required".into());
    }
    for (key, s) in &f.selections {
        if !valid_key(key) || !selection_valid(s) {
            add(format!("{key}: invalid artwork selection"));
        }
    }
    for route in &f.input_routes {
        if !selection_valid(&route.when)
            || !["Indigo", "Screen"].contains(&route.destination.as_str())
            || !["source", "derived"].contains(&route.name.as_str())
        {
            add("Invalid input route".into());
        }
    }
    let rules: BTreeMap<_, _> = r.rules.iter().map(|x| (x.rule_key.as_str(), x)).collect();
    let links: BTreeMap<_, _> = r.file_links.iter().map(|l| (l.key.as_str(), l)).collect();
    let mut dependencies: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for rule in &r.rules {
        let key = rule.rule_key.as_str();
        let treatment = f.treatments.get(key);
        let policy = f.policies.get(key);
        if !valid_key(key) || rule.sources.len() > 30 || rule.protection.len() > 30 {
            add(format!("{key}: invalid key or source count"));
        }
        if rule.operation == "policy" {
            if policy.is_none() || treatment.is_some() {
                add(format!("{key}: policy requires exactly one policy definition"));
            }
        } else {
            if rule.sources.is_empty() {
                add(format!("{key}: shape source required"));
            }
            if treatment.is_none() || policy.is_some() {
                add(format!("{key}: shape requires exactly one treatment"));
            }
        }
        if !["spread", "choke"].contains(&rule.operation.as_str()) && rule.distance_mm.is_some() {
            add(format!("{key}: only spread/choke may have a distance"));
        }
        if rule.distance_mm.is_some_and(|n| !n.is_finite() || !(0.0..=10.0).contains(&n)) {
            add(format!("{key}: invalid distance"));
        }
        if let Some(t) = treatment {
            if !selection_valid(&t.select)
                || !paint_valid(&t.paint)
                || !["all", "shared_only", "except_shared"].contains(&t.edges.as_str())
                || !["none", "exclude"].contains(&t.protection.as_str())
                || !["union", "object", "ink"].contains(&t.partition.as_str())
                || !["full_shape", "outer_band"].contains(&t.result.as_str())
            {
                add(format!("{key}: unsupported treatment"));
            }
            if (t.protection == "exclude") != !rule.protection.is_empty() || (t.edges == "all") != t.neighbours.is_empty() || t.neighbours.len() > 20
            {
                add(format!("{key}: protection/neighbour semantics disagree"));
            }
            if (t.result == "outer_band" && rule.operation != "spread")
                || (rule.operation == "combine" && (t.partition != "union" || t.result != "full_shape"))
            {
                add(format!("{key}: incompatible shape result"));
            }
        }
        for s in rule.sources.iter().chain(&rule.protection).chain(treatment.into_iter().flat_map(|t| &t.neighbours)) {
            match s.kind.as_str() {
                "artwork" => {
                    if !f.selections.get(&s.key).is_some_and(selection_valid) {
                        add(format!("{key}: missing selection {}", s.key));
                    }
                }
                "rule" => {
                    if !rules
                        .get(s.key.as_str())
                        .is_some_and(|parent| parent.operation != "policy" && (parent.garment == "any" || parent.garment == rule.garment))
                    {
                        add(format!("{key}: invalid shape dependency {}", s.key));
                    }
                    dependencies.entry(key).or_default().push(&s.key);
                }
                _ => add(format!("{key}: unknown source kind {}", s.kind)),
            }
        }
        match policy {
            Some(Policy::Assignment { when, role, paint }) => {
                if !selection_valid(when) || role.as_ref().is_some_and(|s| !valid_key(s)) || !paint_valid(paint) {
                    add(format!("{key}: invalid assignment policy"));
                }
            }
            Some(Policy::Operator { when, decision }) => {
                if !selection_valid(when) || !valid_key(decision) {
                    add(format!("{key}: invalid operator policy"));
                }
            }
            Some(Policy::Reference { covers }) => {
                if covers.is_empty() || covers.len() > 100 {
                    add(format!("{key}: reference policy needs covered treatments"));
                }
                for covered in covers {
                    if covered != "$colour"
                        && !rules.get(covered.as_str()).is_some_and(|parent| {
                            parent.operation != "policy"
                                && f.treatments.contains_key(covered)
                                && (rule.garment == "any" || parent.garment == "any" || parent.garment == rule.garment)
                        })
                    {
                        add(format!("{key}: invalid covered treatment {covered}"));
                    }
                }
            }
            None => {}
        }
    }
    for key in f.treatments.keys().chain(f.policies.keys()) {
        if !rules.contains_key(key.as_str()) {
            add(format!("Unknown construction reference {key}"));
        }
    }
    for l in &r.file_links {
        let key = &l.key;
        let t = f.treatments.get(&l.source_rule);
        let o = f.outputs.get(key);
        if !valid_key(key)
            || !valid_key(&l.pdf_layer)
            || !["exact", "source"].contains(&l.name_mode.as_str())
            || !["Screen", "Indigo"].contains(&l.destination.as_str())
            || (l.name_mode == "exact") != l.separation_name.is_some()
            || l.separation_name.as_ref().is_some_and(|s| !valid_key(s))
            || l.layer_order > 1000
        {
            add(format!("{key}: invalid file-link fields"));
        }
        if !rules.get(l.source_rule.as_str()).is_some_and(|p| p.operation != "policy" && (p.garment == "any" || p.garment == l.garment)) {
            add(format!("{key}: invalid file source"));
        }
        if !o.is_some_and(|o| ["all", "matching_output_ink"].contains(&o.select.as_str()) && o.emit == "when_nonempty") {
            add(format!("{key}: invalid output selection"));
        }
        if o.is_some_and(|o| o.select == "matching_output_ink") && (l.name_mode != "exact" || !t.is_some_and(|t| t.partition == "ink")) {
            add(format!("{key}: exact ink partition required"));
        }
        if t.is_some_and(|t| t.partition == "ink") && l.name_mode == "exact" && !o.is_some_and(|o| o.select == "matching_output_ink") {
            add(format!("{key}: ink output must select its own ink"));
        }
        if t.is_some_and(|t| matches!(t.paint, Paint::OutputInk)) && l.name_mode != "exact" {
            add(format!("{key}: output paint needs exact ink"));
        }
    }
    for key in f.outputs.keys() {
        if !links.contains_key(key.as_str()) {
            add(format!("Unknown file output {key}"));
        }
    }
    for e in &f.file.before {
        if !links.get(e.lower.as_str()).zip(links.get(e.upper.as_str())).is_some_and(|(a, b)| a.layer_order < b.layer_order) {
            add(format!("Invalid file order {}/{}", e.lower, e.upper));
        }
    }
    // Iterative reachability includes sources, protection and neighbours. Bounded by 100 rules.
    for start in dependencies.keys() {
        let mut todo = dependencies.get(start).cloned().unwrap_or_default();
        let mut seen = BTreeSet::new();
        while let Some(k) = todo.pop() {
            if k == *start {
                add(format!("Cyclic construction dependency {start}"));
                break;
            }
            if seen.insert(k) {
                todo.extend(dependencies.get(k).into_iter().flatten().copied());
            }
        }
    }
    if f.colour.rgb != "preserve"
        || f.colour.cmyk != "preserve"
        || f.colour.spots != "preserve_names_and_tints"
        || f.colour.untagged != "retain_and_report"
    {
        add("Unsupported colour contract".into());
    }
    if f.geometry.units != "mm"
        || f.geometry.edges != "hard"
        || f.geometry.raster_outline != "explicit_prepared_mask"
        || f.geometry.paint_extension != "extend_source"
        || f.geometry.overlap != "union_coverage"
        || f.geometry.coordinates != "shared_page_space"
    {
        add("Unsupported geometry contract".into());
    }
    if f.file.editable_groups != "preserve"
        || f.file.indigo != "composite"
        || f.file.preserve.is_empty()
        || f.file.before.len() > 100
        || f.file.preserve.iter().any(|x| {
            !["page_boxes", "page_origin", "profiles", "ink_values", "spot_names", "spot_tints", "clipping", "transparency", "overprint"]
                .contains(&x.as_str())
        })
    {
        add("Unsupported file-preservation contract".into());
    }
    if [&f.checks.empty_required_output, &f.checks.unexpected_output, &f.checks.unknown_assignment, &f.checks.stale_result]
        .iter()
        .any(|s| s.as_str() != "block")
        || f.checks.geometry_tolerance_mm.is_some_and(|n| !n.is_finite() || !(0.0..=1.0).contains(&n))
    {
        add("Unsupported acceptance contract".into());
    }
    if let Some(v) = &f.verification {
        errors.extend(verification_issues(v, r));
    }
    errors
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    pub(crate) fn recipe() -> Recipe {
        let mut r = crate::tests::request().recipe;
        r.foundation = Some(json!({
            "version":1,
            "selections":{"selected":[[]]},
            "input_routes":[{"when":[[]],"destination":"Screen","name":"derived"}],
            "treatments":{"arbitrary":{"select":[[]],"edges":"all","neighbours":[],"protection":"none","paint":{"mode":"output_ink"},"partition":"union","result":"full_shape"}},
            "policies":{},"outputs":{"output":{"select":"all","emit":"when_nonempty"}},
            "colour":{"rgb":"preserve","cmyk":"preserve","spots":"preserve_names_and_tints","untagged":"retain_and_report"},
            "geometry":{"units":"mm","edges":"hard","raster_outline":"explicit_prepared_mask","paint_extension":"extend_source","overlap":"union_coverage","coordinates":"shared_page_space"},
            "file":{"before":[],"preserve":["page_boxes"],"editable_groups":"preserve","indigo":"composite"},
            "checks":{"geometry_tolerance_mm":null,"empty_required_output":"block","unexpected_output":"block","unknown_assignment":"block","stale_result":"block"},
            "verification":{"version":1,"plates":[
                {"key":"dark-output","garment":"dark","name":"Exact ink","name_mode":"exact","destination":"Screen","required":true,"links":["output"],"evidence":"synthetic specification"},
                {"key":"light-output","garment":"light","name":"Exact ink","name_mode":"exact","destination":"Screen","required":true,"links":["output"],"evidence":"synthetic specification"}
            ],"contributions":{"output":{"emptiness":"conditional_on_proven_source"}},"acceptance":{}}
        }));
        r
    }
    #[test]
    fn complete_contract_and_mandatory_plate_are_distinct_from_optional_contribution() {
        let r = recipe();
        assert!(issues(&r).is_empty(), "{:?}", issues(&r));
        assert!(r.contract_issues(crate::recipe::Garment::Dark).is_empty());
        let mut slots = vec![];
        r.enumerate(crate::recipe::Garment::Dark, &mut slots).unwrap();
        assert!(slots.iter().find(|s| s.key == "file/output/contribution").unwrap().conditional);
        assert!(!slots.iter().find(|s| s.key == "plate/dark-output").unwrap().conditional);
        assert!(!slots.iter().any(|s| s.key.starts_with("legacy-plate/")));
    }
    #[test]
    fn broken_migration_cannot_remove_a_required_plate() {
        for replacement in [
            json!([]),
            json!([{"key":"output","garment":"dark","name":"Exact ink","name_mode":"exact","destination":"Screen","required":false,"links":["output"],"evidence":"synthetic"}]),
        ] {
            let mut r = recipe();
            r.foundation.as_mut().unwrap()["verification"]["plates"] = replacement;
            assert!(!issues(&r).is_empty());
            let mut slots = vec![];
            r.enumerate(crate::recipe::Garment::Dark, &mut slots).unwrap();
            assert!(slots.iter().any(|s| s.key == "legacy-plate/Exact ink" && !s.conditional));
        }
    }
    #[test]
    fn valid_garment_specific_plate_does_not_invent_other_garment_requirement() {
        let mut r = recipe();
        r.file_links[0].garment = "light".into();
        r.rules[0].garment = "light".into();
        r.foundation.as_mut().unwrap()["verification"]["plates"].as_array_mut().unwrap().remove(0);
        assert!(issues(&r).is_empty(), "{:?}", issues(&r));
        let mut slots = vec![];
        r.enumerate(crate::recipe::Garment::Dark, &mut slots).unwrap();
        assert!(slots.is_empty());
    }
    #[test]
    fn cycles_include_neighbours_and_protection() {
        for relation in ["sources", "protection", "neighbours"] {
            let mut r = recipe();
            let dependency = Source { kind: "rule".into(), key: "arbitrary".into() };
            match relation {
                "sources" => r.rules[0].sources.push(dependency),
                "protection" => r.rules[0].protection.push(dependency),
                _ => r.foundation.as_mut().unwrap()["treatments"]["arbitrary"]["neighbours"] = json!([{"kind":"rule","key":"arbitrary"}]),
            }
            assert!(issues(&r).iter().any(|s| s.contains("Cyclic")), "{relation}: {:?}", issues(&r));
        }
    }
    #[test]
    fn malformed_fields_and_unknown_dependencies_block_contract() {
        for change in 0..6 {
            let mut r = recipe();
            match change {
                0 => r.rules[0].sources[0].kind = "future".into(),
                1 => r.file_links[0].name_mode = "fuzzy".into(),
                2 => r.file_links[0].destination = "anywhere".into(),
                3 => r.foundation.as_mut().unwrap()["treatments"]["arbitrary"]["result"] = json!("future"),
                4 => r.foundation.as_mut().unwrap()["treatments"]["arbitrary"]["unexpected"] = json!(true),
                _ => {
                    r.foundation.as_mut().unwrap()["treatments"]["arbitrary"]["select"] =
                        json!([[{"field":"unknown","operator":"equals","value":"x"}]])
                }
            }
            assert!(!issues(&r).is_empty(), "mutation {change}");
        }
    }
    #[test]
    fn predicate_schema_is_checked_before_evaluation() {
        assert!(selection_valid(&json!([[]])));
        assert!(!selection_valid(&json!([[{"field":"role","operator":"equals","value":"x"},{"field":"future","operator":"equals","value":1}]])));
        assert!(!selection_valid(&json!([[{"field":"black","operator":"equals","value":"false"}]])));
    }
    #[test]
    fn numerical_criteria_cannot_silently_change_units() {
        let mut r = recipe();
        for (key, value, units, valid) in [
            ("geometry_tolerance_mm", 0.02, "mm", true),
            ("geometry_tolerance_mm", 0.02, "fraction", false),
            ("tone_fraction", 0.5, "fraction", true),
            ("tone_fraction", 0.5, "mm", false),
            ("tone_fraction", 1.1, "fraction", false),
        ] {
            r.foundation.as_mut().unwrap()["verification"]["acceptance"] = json!({
                key: {"value":value,"units":units,"approved_by":"Synthetic author","approved_at":"2026-10-18","evidence":"Synthetic criterion"}
            });
            assert_eq!(issues(&r).is_empty(), valid, "{key} {units}: {:?}", issues(&r));
        }
    }
}
