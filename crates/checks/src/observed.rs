//! Final-separation observations. These never stand in for original artwork fidelity.
use crate::pdf::{Census, PaintedObject};
use crate::{Evidence, Obligation, Owner, Report, Request, Status};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use vectorcraft_geom::Shape;

pub(crate) fn enumerate(request: &Request, out: &mut Vec<Obligation>) {
    let mut add = |key: String, label: String, expected: Value, rule: Option<String>| {
        out.push(Obligation {
            key,
            area: "Observed separation construction".into(),
            label,
            rule,
            file_link: None,
            plate: None,
            owner: Owner::Engineering,
            expected,
            conditional: false,
        });
    };
    add("observed/routing".into(), "All observed paint uses declared layer and ink routes".into(), json!(request.recipe.file_links), None);
    for link in request.recipe.file_links.iter().filter(|l| request.garment.matches(&l.garment)) {
        for facet in ["paint", "order"] {
            add(format!("observed/file/{}/{facet}", link.key), format!("{}: observed {facet}", link.pdf_layer), json!(link), None);
        }
    }
    if let Some(plates) = request.recipe.foundation.as_ref().and_then(|f| f.pointer("/verification/plates")).and_then(Value::as_array) {
        for p in plates.iter().filter(|p| p.get("garment").and_then(Value::as_str).is_some_and(|g| request.garment.matches(g))) {
            if let Some(key) = p.get("key").and_then(Value::as_str) {
                add(
                    format!("observed/plate/{key}/presence"),
                    format!("{}: physical output presence", p.get("name").and_then(Value::as_str).unwrap_or("Indigo composite")),
                    p.clone(),
                    None,
                );
            }
        }
    }
    for rule in request
        .recipe
        .rules
        .iter()
        .filter(|r| request.garment.matches(&r.garment) && !matches!(r.operation.as_str(), "policy" | "preserve" | "combine"))
    {
        add(
            format!("observed/rule/{}/relationship", rule.rule_key),
            format!("{}: final plate relationship", rule.title),
            json!(rule),
            Some(rule.rule_key.clone()),
        );
    }
    for item in out.iter_mut().filter(|o| o.key.starts_with("observed/")) {
        item.conditional = item.key.starts_with("observed/file/")
            || item.key.starts_with("observed/rule/")
            || (item.key.starts_with("observed/plate/") && item.expected.get("required") == Some(&json!(false)));
    }
}
fn evidence(r: &Request) -> Vec<Evidence> {
    vec![
        Evidence {
            artifact_sha256: r.identity.pdf_sha256.clone(),
            producer: "checks/observed-construction-1".into(),
            detail: "Exact final separation identity; each finding states its census dependencies, without original artwork inference".into(),
        },
        Evidence {
            artifact_sha256: r.identity.recipe_sha256.clone(),
            producer: "checks/recipe-contract-1".into(),
            detail: "Pinned recipe facts; publication/approval remains a separate finding".into(),
        },
    ]
}
fn matches(link: &crate::recipe::FileLink, p: &PaintedObject) -> bool {
    p.layer == link.pdf_layer && (link.name_mode == "source" || p.ink.as_ref() == link.separation_name.as_ref())
}
fn live(p: &PaintedObject) -> bool {
    p.geometry_mm.as_ref().is_some_and(|g| !g.is_empty()) && p.tint != Some(0.0)
}
fn positive_ink(c: &Census, p: &PaintedObject, composite: bool) -> bool {
    if !live(p)
        || p.geometry_mm.as_ref().is_none_or(|g| {
            let area = g.to_bezpath().area().abs();
            !area.is_finite() || area <= 0.0
        })
    {
        return false;
    }
    if p.ink.is_some() {
        if !p.tint.is_some_and(|t| t > 0.0) || matches!(p.ink.as_deref(), Some("None" | "All")) {
            return false;
        }
        if !composite {
            return true;
        }
        // Composite presence requires a positive decoded subtractive alternate;
        // a named colour with a paper-white alternate proves no process ink.
        let color = p.paint.get("color");
        let keys: &[&str] = match color.and_then(|c| c.get("model")).and_then(Value::as_str) {
            Some("cmyk") => &["c", "m", "y", "k"],
            Some("gray") => &["k"],
            _ => return false,
        };
        return keys.iter().any(|k| color.and_then(|c| c.get(k)).and_then(Value::as_f64).is_some_and(|v| v.is_finite() && v > 0.0));
    }
    let source = p.source_space.and_then(|i| c.paint_sources.get(i));
    if let Some(source) = source
        && source.color_space == vectorcraft_pdf::PrintColorSpace::Separation
        && source.colorants.len() == 1
        && source.colorants.first().is_some_and(|n| ["Cyan", "Magenta", "Yellow", "Black"].contains(&n.as_str()))
    {
        // Direct subtractive process colourants retain their named component;
        // their alternate is not a process-colour conversion oracle. Reuse the
        // raw-space tonal reconciliation, including its component validation.
        return crate::tone::source_tones(p, source).is_ok_and(|tones| tones.values().any(|t| *t > 0.0));
    }
    let color = p.paint.get("color");
    let keys: &[&str] = match source.map(|s| s.color_space) {
        Some(vectorcraft_pdf::PrintColorSpace::DeviceCmyk) => &["c", "m", "y", "k"],
        Some(vectorcraft_pdf::PrintColorSpace::DeviceGray) => &["k"],
        _ => return false, // RGB/profile-dependent ink presence needs qualified conversion.
    };
    keys.iter().any(|k| color.and_then(|v| v.get(k)).and_then(Value::as_f64).is_some_and(|v| v.is_finite() && v > 0.0))
}
#[derive(Default)]
struct Survival {
    knockout: BTreeMap<usize, usize>,
    process: BTreeMap<usize, usize>,
    zero: BTreeMap<(usize, Option<String>), usize>,
}
impl Survival {
    fn new(c: &Census) -> Self {
        let mut out = Self::default();
        for p in &c.paints {
            if let Some(page) = p.page
                && p.geometry_mm.as_ref().is_some_and(|g| !g.is_empty())
            {
                if p.process {
                    out.process.entry(page).and_modify(|v| *v = (*v).max(p.order)).or_insert(p.order);
                }
                if !p.overprint {
                    out.knockout.entry(page).and_modify(|v| *v = (*v).max(p.order)).or_insert(p.order);
                }
                if p.tint == Some(0.0) {
                    out.zero.entry((page, p.ink.clone())).and_modify(|v| *v = (*v).max(p.order)).or_insert(p.order);
                }
            }
        }
        out
    }
    fn composite_unmodified(&self, p: &PaintedObject) -> bool {
        self.unmodified(p) && p.page.is_some_and(|page| self.process.get(&page).is_none_or(|last| *last <= p.order))
    }
    fn unmodified(&self, p: &PaintedObject) -> bool {
        p.page.is_some_and(|page| {
            (!p.process || self.process.get(&page).is_none_or(|last| *last <= p.order))
                && self.knockout.get(&page).is_none_or(|last| *last <= p.order)
                && self.zero.get(&(page, p.ink.clone())).is_none_or(|last| *last <= p.order)
        })
    }
}
fn sample(p: &PaintedObject) -> Value {
    json!({"object":p.id,"page":p.page,"layer":p.layer,"ink":p.ink,"process":p.process,"tint":p.tint,"overprint":p.overprint,"order":p.order,"bounds_mm":p.bounds_mm})
}
fn reason(report: &mut Report, r: &Request, why: &str) -> Result<(), String> {
    let keys: Vec<_> = report.findings.iter().filter(|f| f.obligation.key.starts_with("observed/")).map(|f| f.obligation.key.clone()).collect();
    for key in keys {
        report.record(&key, Status::Incomplete, why, Value::Null, evidence(r))?;
    }
    Ok(())
}

pub(crate) fn check_pdf(
    r: &Request,
    bytes: &[u8],
    doc: &vectorcraft_doc::Document,
    warnings: &[String],
    c: &Census,
    report: &mut Report,
) -> Result<(), String> {
    if c.image_objects > 0 {
        match crate::observed_images::Prelude::new(r, bytes, doc, warnings, c) {
            Ok(proof) => check(r, proof.census(), Some(&proof), report),
            Err(why) => reason(report, r, &format!("Technical-vector isolation from process images is unresolved: {why}")),
        }
    } else {
        check(r, c, None, report)
    }
}
fn check(r: &Request, c: &Census, prelude: Option<&crate::observed_images::Prelude>, report: &mut Report) -> Result<(), String> {
    reason(report, r, "This final-output observation has not yet established its specific prerequisites")?;
    if !crate::contract::issues(&r.recipe).is_empty() || crate::digest(&r.recipe)? != r.identity.recipe_sha256 {
        return reason(report, r, "Executable recipe contract/identity must be valid before interpreting final-output requirements");
    }
    if r.input_class.as_ref().is_none_or(|f| !f.evidenced() || f.value != json!("individual_separation"))
        || r.assignments_digest()? != r.identity.assignments_sha256
    {
        return reason(report, r, "An evidenced individual-separation scope and matching job-assignment identity are required");
    }
    if !c.paint_order_verified
        || c.raw_audit_error.is_some()
        || (prelude.is_none() && c.raw_audit.as_ref().is_none_or(|a| a.issue_count != 0 || a.image_use_count != 0))
        || c.budget_exhausted
        || !c.unresolved.is_empty()
        || !c.unaccounted_objects.is_empty()
        || c.paints.iter().any(|p| p.page.is_none() || p.geometry_mm.is_none())
    {
        return reason(report, r, "Complete raw/native printable coverage and order reconciliation is required for these final-output observations");
    }
    let links: Vec<_> = r.recipe.file_links.iter().filter(|l| r.garment.matches(&l.garment)).collect();
    let paints: Vec<_> = c.paints.iter().filter(|p| p.geometry_mm.as_ref().is_some_and(|g| !g.is_empty())).collect();
    // Bound the remaining small contract-to-object joins before recording results.
    // Order itself uses precomputed ranges and an independent comparison budget.
    let plate_count = r.recipe.foundation.as_ref().and_then(|f| f.pointer("/verification/plates")).and_then(Value::as_array).map_or(0, Vec::len);
    let join_work = paints.len().saturating_mul(links.len()).saturating_mul(
        links.len().saturating_mul(3).saturating_add(plate_count.saturating_mul(2)).saturating_add(r.recipe.rules.len().saturating_mul(2)),
    );
    if join_work > 20_000_000 {
        return reason(report, r, "Final-output contract/object join budget exceeded; observations were not qualified");
    }
    let survival = Survival::new(c);
    let exact_names: BTreeSet<_> = links.iter().filter(|l| l.name_mode == "exact").filter_map(|l| l.separation_name.as_deref()).collect();
    let ambiguous = |p: &PaintedObject| {
        p.ink.as_deref().is_some_and(|name| exact_names.contains(name)) && links.iter().any(|l| l.name_mode == "source" && matches(l, p))
    };
    let ambiguous_count = paints.iter().filter(|p| ambiguous(p)).count();
    let mut ranges: BTreeMap<&str, BTreeMap<usize, (usize, usize)>> = BTreeMap::new();
    for link in &links {
        let mut pages: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
        for p in paints.iter().filter(|p| matches(link, p)) {
            if let Some(page) = p.page {
                pages
                    .entry(page)
                    .and_modify(|v| {
                        v.0 = v.0.min(p.order);
                        v.1 = v.1.max(p.order);
                    })
                    .or_insert((p.order, p.order));
            }
        }
        ranges.insert(&link.key, pages);
    }

    let wrong: Vec<_> = paints.iter().filter(|p| !links.iter().any(|l| matches(l, p))).collect();
    report.record("observed/routing",if !wrong.is_empty(){Status::Fail}else if ambiguous_count>0 || prelude.is_some(){Status::Incomplete}else{Status::Pass},
        "Observed nonempty paint checked against declared exact-ink or retained-source layer routes; source colour fidelity remains separate",
        json!({"image_prelude":prelude.map(|p|p.summary()),"paint_count":paints.len(),"ambiguous_retained_source_count":ambiguous_count,"wrong_count":wrong.len(),"wrong_sample":wrong.iter().take(64).map(|p|sample(p)).collect::<Vec<_>>(),"observed_inks":paints.iter().filter_map(|p|p.ink.as_ref()).collect::<BTreeSet<_>>() }),evidence(r))?;
    let mut order_work = 0usize;
    for link in &links {
        let image_affected = prelude.is_some_and(|p| p.affects(&link.key));
        let objects: Vec<_> = paints.iter().filter(|p| p.layer == link.pdf_layer).copied().collect();
        let incompatible: Vec<_> = objects.iter().filter(|p| !links.iter().any(|l| l.pdf_layer == link.pdf_layer && matches(l, p))).collect();
        let matched: Vec<_> = objects.iter().filter(|p| matches(link, p)).copied().collect();
        let required = r
            .recipe
            .foundation
            .as_ref()
            .and_then(|f| f.pointer("/verification/contributions"))
            .and_then(|v| v.get(&link.key))
            .and_then(|v| v.get("emptiness"))
            .and_then(Value::as_str)
            == Some("required");
        let flags_ambiguous =
            matched.iter().any(|p| ambiguous(p) || links.iter().any(|other| other.overprint != link.overprint && matches(other, p)));
        let wrong_flags: Vec<_> = matched
            .iter()
            .filter(|p| p.overprint != link.overprint && !links.iter().any(|other| other.overprint != link.overprint && matches(other, p)))
            .collect();
        let status = if !incompatible.is_empty() || !wrong_flags.is_empty() || (required && !image_affected && matched.iter().all(|p| !live(p))) {
            Status::Fail
        } else if image_affected || matched.is_empty() || flags_ambiguous {
            Status::Incomplete
        } else {
            Status::Pass
        };
        report.record(&format!("observed/file/{}/paint",link.key),status,
            if image_affected{"Image contribution remains unresolved; vector defects are independent witnesses"}else if required && matched.iter().all(|p|!live(p)){"Unconditionally required contribution has no nonzero paint"}else if flags_ambiguous{"Shared source/exact route prevents contributor-specific overprint attribution"}else if matched.is_empty() && incompatible.is_empty(){"No matching nonempty paint observed; conditional contribution applicability requires source evidence"}else{"Observed route and overprint flags; shared-plate contributors are not individually attributed"},
            json!({"expected_layer":link.pdf_layer,"expected_ink":link.separation_name,"expected_overprint":link.overprint,"matched_count":matched.len(),"incompatible_count":incompatible.len(),"wrong_overprint_count":wrong_flags.len(),"sample":objects.iter().take(64).map(|p|sample(p)).collect::<Vec<_>>()}),evidence(r))?;
        let mut comparisons = vec![];
        let mut bad = 0;
        let pages = ranges.get(link.key.as_str()).ok_or("Output order index missing")?;
        let mut checked = 0usize;
        let mut order_limited = false;
        'comparisons: for other in links.iter().filter(|o| o.pdf_layer != link.pdf_layer && o.layer_order != link.layer_order) {
            let other_pages = ranges.get(other.key.as_str()).ok_or("Other output order index missing")?;
            for (page, (amin, amax)) in pages {
                order_work += 1;
                if order_work > 1_000_000 {
                    order_limited = true;
                    break 'comparisons;
                }
                if let Some((bmin, bmax)) = other_pages.get(page) {
                    checked += 1;
                    let correct = if link.layer_order < other.layer_order { amax < bmin } else { bmax < amin };
                    bad += usize::from(!correct);
                    if comparisons.len() < 128 {
                        comparisons
                            .push(json!({"page":page,"other":other.key,"actual_range":[amin,amax],"other_range":[bmin,bmax],"correct":correct}));
                    }
                }
            }
        }
        report.record(
            &format!("observed/file/{}/order", link.key),
            if bad > 0 {
                Status::Fail
            } else if prelude.is_some() || matched.is_empty() || flags_ambiguous || order_limited {
                Status::Incomplete
            } else {
                Status::Pass
            },
            "Actual vector order between observed outputs on each page; image order and absent conditional outputs remain unresolved",
            json!({"comparisons":comparisons,"comparisons_checked":checked,"wrong_count":bad,"budget_exhausted":order_limited}),
            evidence(r),
        )?;
    }
    if let Some(plates) = r.recipe.foundation.as_ref().and_then(|f| f.pointer("/verification/plates")).and_then(Value::as_array) {
        for plate in plates.iter().filter(|p| p.get("garment").and_then(Value::as_str).is_some_and(|g| r.garment.matches(g))) {
            let Some(key) = plate.get("key").and_then(Value::as_str) else {
                continue;
            };
            let members: BTreeSet<_> = plate.get("links").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
            let objects: Vec<_> = paints
                .iter()
                .filter(|p| {
                    if !live(p) {
                        false
                    } else if plate.get("name_mode").and_then(Value::as_str) == Some("exact") {
                        p.ink.as_deref() == plate.get("name").and_then(Value::as_str)
                    } else {
                        links.iter().any(|l| members.contains(l.key.as_str()) && matches(l, p))
                    }
                })
                .collect();
            let routed_count = objects.iter().filter(|p| links.iter().any(|l| members.contains(l.key.as_str()) && matches(l, p))).count();
            let image_affected = prelude.is_some_and(|p| members.iter().any(|key| p.affects(key)));
            let required = plate.get("required").and_then(Value::as_bool) == Some(true);
            let surviving = objects.iter().any(|p| {
                let composite = plate.get("name_mode").and_then(Value::as_str) == Some("composite");
                positive_ink(c, p, composite) && if composite { survival.composite_unmodified(p) } else { survival.unmodified(p) }
            });
            report.record(&format!("observed/plate/{key}/presence"),if surviving{Status::Pass}else if required && !image_affected && objects.is_empty(){Status::Fail}else{Status::Incomplete},
                if objects.is_empty(){"Required output absence fails; conditional output absence needs source applicability"}else{"Presence requires positive decoded ink and filled area; unsupported colour conversion or later knockout, zero-tint or process paint remains unresolved"},
                json!({"required":required,"name":plate.get("name"),"destination":plate.get("destination"),"contributing_layers":objects.iter().map(|p|p.layer.as_str()).collect::<BTreeSet<_>>(),"objects":objects.len(),"routed_objects":routed_count,"unmodified_nonempty_paint":surviving}),evidence(r))?;
        }
    }
    relationships(r, c, &survival, report)?;
    let absent: Vec<_> = report.findings.iter().filter(|f| f.status == Status::NotApplicable).map(|f| f.obligation.key.clone()).collect();
    for key in absent {
        let targets = if let Some(link_key) = key.strip_prefix("file/").and_then(|k| k.strip_suffix("/contribution")) {
            if prelude.is_some_and(|p| p.affects(link_key))
                || links.iter().find(|l| l.key == link_key).is_some_and(|l| paints.iter().any(|p| matches(l, p)))
            {
                continue;
            }
            vec![format!("observed/file/{link_key}/paint"), format!("observed/file/{link_key}/order")]
        } else if let Some(rule_key) = key.strip_prefix("rule/").and_then(|k| k.strip_suffix("/applicability")) {
            if links
                .iter()
                .filter(|l| l.source_rule == rule_key)
                .any(|l| prelude.is_some_and(|p| p.affects(&l.key)) || paints.iter().any(|p| matches(l, p)))
            {
                continue;
            }
            vec![format!("observed/rule/{rule_key}/relationship")]
        } else if let Some(plate_key) = key.strip_prefix("plate/") {
            if prelude.is_some() {
                continue;
            }
            vec![format!("observed/plate/{plate_key}/presence")]
        } else {
            continue;
        };
        for target in targets {
            if report.findings.iter().any(|f| f.obligation.key == target && f.obligation.conditional && f.status == Status::Incomplete) {
                report.record(
                    &target,
                    Status::NotApplicable,
                    "Underlying conditional contribution/rule has proven empty source; no observed result was waived",
                    json!({"source_proof":key}),
                    evidence(r),
                )?;
            }
        }
    }
    Ok(())
}

// A named combined rule can be recovered from a whole shared output, while its
// individual coalesced contributors cannot. No content is invented from absence.
fn anchor<'a>(r: &Request, c: &'a Census, survival: &Survival, key: &str) -> Result<Vec<&'a PaintedObject>, String> {
    let rule = r.recipe.rules.iter().find(|x| x.rule_key == key && r.garment.matches(&x.garment)).ok_or("Rule absent")?;
    let direct: Vec<_> = r.recipe.file_links.iter().filter(|l| r.garment.matches(&l.garment) && l.source_rule == key).collect();
    let t = r.recipe.foundation.as_ref().and_then(|f| f.get("treatments")).and_then(|t| t.get(key)).ok_or("Anchor treatment missing")?;
    if direct.is_empty() && (t.get("select") != Some(&json!([[]])) || t.get("result") != Some(&json!("full_shape")) || !rule.protection.is_empty()) {
        return Err("Combined anchor must preserve every declared contribution without selection or protection".into());
    }
    let wanted: BTreeSet<&str> = if direct.is_empty() && rule.operation == "combine" && rule.sources.iter().all(|s| s.kind == "rule") {
        rule.sources.iter().map(|s| s.key.as_str()).collect()
    } else if !direct.is_empty() {
        BTreeSet::from([key])
    } else {
        return Err("Source rule has no directly observable final output".into());
    };
    let links: Vec<_> = r.recipe.file_links.iter().filter(|l| r.garment.matches(&l.garment) && wanted.contains(l.source_rule.as_str())).collect();
    if links.is_empty() || wanted.iter().any(|s| !links.iter().any(|l| l.source_rule == *s)) {
        return Err("Every combined source needs an explicit output link".into());
    }
    let routes: BTreeSet<_> = links.iter().map(|l| (&l.pdf_layer, &l.separation_name)).collect();
    if routes.len() != 1 {
        return Err("Anchor spans multiple final outputs; a qualified union mapping is required".into());
    }
    for l in &links {
        if r.recipe.foundation.as_ref().and_then(|f| f.get("outputs")).and_then(|o| o.get(&l.key)).and_then(|o| o.get("select"))
            != Some(&json!("all"))
        {
            return Err("Anchor output filters its contributing coverage".into());
        }
        if l.name_mode != "exact"
            || r.recipe
                .file_links
                .iter()
                .filter(|x| {
                    r.garment.matches(&x.garment) && x.pdf_layer == l.pdf_layer && (x.name_mode == "source" || x.separation_name == l.separation_name)
                })
                .any(|x| !wanted.contains(x.source_rule.as_str()))
        {
            return Err("Final plate coalesces other contributors or retains unidentified source paint".into());
        }
    }
    if c.paints.iter().any(|p| live(p) && links.iter().any(|l| p.ink.as_ref() == l.separation_name.as_ref()) && !links.iter().any(|l| matches(l, p)))
    {
        return Err("Technical ink also prints outside the anchor routes; the complete physical plate is not isolated".into());
    }
    let objects: Vec<_> = c.paints.iter().filter(|p| live(p) && links.iter().any(|l| matches(l, p))).collect();
    if objects.is_empty() {
        return Err("No nonempty final anchor; missing output cannot prove source absence".into());
    }
    if objects.iter().any(|p| p.tint != Some(1.0) || !survival.unmodified(p)) {
        return Err("Relationship requires fully tinted technical coverage without later knockout/zero-tint interference".into());
    }
    Ok(objects)
}
/// Distance to a fixed closed boundary is 1-Lipschitz. Every nondegenerate
/// round spread/choke boundary is at its prescribed distance from the source
/// boundary, so a gap discrepancy gives a lower bound on target deviation.
/// Matching unsigned distances cannot establish side, topology or completeness.
fn gap_witness(gap: &crate::geometry::GapRange, distance: f64, tolerance: Option<f64>) -> (f64, Status) {
    let lower = (distance - gap.minimum_mm[1]).max(gap.maximum_mm[0] - distance).max(0.0);
    let status = if tolerance.is_some_and(|t| lower > t) { Status::Fail } else { Status::Incomplete };
    (lower, status)
}
fn relationships(r: &Request, c: &Census, survival: &Survival, report: &mut Report) -> Result<(), String> {
    let tolerance =
        r.acceptance.get("geometry_tolerance_mm").filter(|f| f.evidenced()).and_then(|f| f.value.as_f64()).filter(|n| n.is_finite() && *n >= 0.0);
    let tolerance = if crate::digest(&r.acceptance)? == r.identity.measurement_sha256 { tolerance } else { None };
    for rule in r.recipe.rules.iter().filter(|x| r.garment.matches(&x.garment) && matches!(x.operation.as_str(), "spread" | "choke")) {
        let key = format!("observed/rule/{}/relationship", rule.rule_key);
        let result = (|| -> Result<(Value, f64, Status), String> {
            let t = r.recipe.foundation.as_ref().and_then(|f| f.get("treatments")).and_then(|t| t.get(&rule.rule_key)).ok_or("Treatment missing")?;
            if rule.sources.len() != 1
                || rule.sources.first().is_none_or(|s| s.kind != "rule")
                || !rule.protection.is_empty()
                || rule.join.as_deref() != Some("round")
                || t.get("edges") != Some(&json!("all"))
                || t.get("partition") != Some(&json!("union"))
                || t.get("result") != Some(&json!("full_shape"))
                || t.get("select") != Some(&json!([[]]))
            {
                return Err(
                    "This rule needs source selection, protected/shared edges or original artwork; a final plate alone cannot establish it".into()
                );
            }
            let source_key = &rule.sources.first().ok_or("Source missing")?.key;
            let source = anchor(r, c, survival, source_key)?;
            let target = anchor(r, c, survival, &rule.rule_key)?;
            let distance = rule.distance_mm.ok_or("Distance missing")?;
            if source
                .iter()
                .chain(&target)
                .filter_map(|p| p.geometry_mm.as_ref())
                .map(|g| g.anchor_count())
                .try_fold(0usize, |n, x| n.checked_add(x))
                .is_none_or(|n| n > 100_000)
            {
                return Err("Final relationship aggregate anchor budget exceeded".into());
            }
            let mut rows = vec![];
            let mut uncertainty = 0.0f64;
            let mut status = if tolerance.is_some() { Status::Pass } else { Status::Incomplete };
            for page in source.iter().chain(&target).filter_map(|p| p.page).collect::<BTreeSet<_>>() {
                let a = crate::program::union(source.iter().filter(|p| p.page == Some(page)).filter_map(|p| p.geometry_mm.clone()))?;
                let b = crate::program::union(target.iter().filter(|p| p.page == Some(page)).filter_map(|p| p.geometry_mm.clone()))?;
                let expected =
                    vectorcraft_pathops::try_offset_path_for_measurement(&a, if rule.operation == "choke" { -distance } else { distance }, 0.00001)
                        .map_err(|e| e.to_string())?;
                let error = source.iter().chain(&target).map(|p| p.geometry_error_mm).fold(0.0, f64::max) * 2.0 + 0.00002;
                if expected.anchor_count() > 100_000 {
                    return Err("Constructed relationship anchor budget exceeded".into());
                }
                if a.is_empty() || b.is_empty() {
                    match crate::geometry::compare(&expected, &b, 0.0, 0.0001) {
                        Ok(m) => {
                            if m.status == Status::Fail {
                                status = Status::Fail;
                            }
                            rows.push(json!({"page":page,"source_empty":a.is_empty(),"target_empty":b.is_empty(),"expected_empty":expected.is_empty(),"nominal_relation_disproved":m.status==Status::Fail,"reason":m.reason}));
                        }
                        Err(why) => {
                            if status != Status::Fail {
                                status = Status::Incomplete;
                            }
                            rows.push(json!({"page":page,"nominal_comparison_limitation":why}));
                        }
                    }
                    continue;
                }

                let mut gap = crate::geometry::boundary_gap(&b, &a, 0.0001)?;
                gap.minimum_mm[0] = (gap.minimum_mm[0] - error).max(0.0);
                gap.minimum_mm[1] += error;
                gap.maximum_mm[0] = (gap.maximum_mm[0] - error).max(0.0);
                gap.maximum_mm[1] += error;
                gap.uncertainty_mm += 2.0 * error;
                let outside = vectorcraft_pathops::try_boolean_for_measurement(
                    &b,
                    vectorcraft_geom::FillRule::NonZero,
                    &a,
                    vectorcraft_geom::FillRule::NonZero,
                    vectorcraft_pathops::BoolOp::Difference,
                )
                .map_err(|e| e.to_string())?;
                let (gap_lower, gap_status) = gap_witness(&gap, distance, tolerance);
                if gap_status == Status::Fail {
                    status = Status::Fail;
                }
                let measured = crate::geometry::compare(&expected, &b, 0.0, 0.0001);
                let (deviation, nominal_disproved, limitation) = match measured {
                    Ok(mut m) => {
                        m.lower_mm = (m.lower_mm - error).max(0.0);
                        m.upper_mm += error;
                        m.uncertainty_mm = m.upper_mm - m.lower_mm;
                        if let Some(tol) = tolerance {
                            let one = if m.expected_components != m.observed_components || m.expected_holes != m.observed_holes || m.lower_mm > tol {
                                Status::Fail
                            } else if m.upper_mm <= tol {
                                Status::Pass
                            } else {
                                Status::Incomplete
                            };
                            if one == Status::Fail || (one == Status::Incomplete && status != Status::Fail) {
                                status = one
                            }
                        }
                        uncertainty = uncertainty.max(m.uncertainty_mm);
                        (
                            json!({"lower":m.lower_mm,"upper":m.upper_mm}),
                            m.lower_mm > 0.0 || m.expected_components != m.observed_components || m.expected_holes != m.observed_holes,
                            Value::Null,
                        )
                    }
                    Err(why) => {
                        if status != Status::Fail {
                            status = Status::Incomplete;
                        }
                        // The independent gap witness survives a nominal topology
                        // limitation. It can disprove a bound tolerance but never Pass
                        // full morphology, even when all observed gaps match.
                        (Value::Null, gap_lower > 0.0, json!(format!("Nominal offset topology unresolved: {why}")))
                    }
                };
                uncertainty = uncertainty.max(gap.uncertainty_mm);
                rows.push(json!({"page":page,"nominal_operation":rule.operation,"nominal_distance_mm":distance,"source_rule":source_key,
                    "target_deviation_mm":deviation,"nominal_relation_disproved":nominal_disproved,"nominal_comparison_limitation":limitation,
                    "target_boundary_to_source_gap":gap,"target_deviation_lower_from_gap_mm":gap_lower,
                    "target_outside_source_area_mm2":outside.to_bezpath().area().abs(),
                    "source_objects":source.iter().filter(|p|p.page==Some(page)).map(|p|p.id).collect::<Vec<_>>(),"target_objects":target.iter().filter(|p|p.page==Some(page)).map(|p|p.id).collect::<Vec<_>>() }));
            }
            Ok((
                json!({"acceptance_mm":tolerance,"measurements":rows,"basis":"final-to-final relationship only; source completeness and construction history are not asserted"}),
                uncertainty,
                status,
            ))
        })();
        match result {
            Ok((observed, error, status)) => {
                let why = if tolerance.is_some() {
                    "Final-to-final relationship measured with bound acceptance; original artwork fidelity remains separate"
                } else {
                    "Final plate relationship measured against the recipe target; manufacturing acceptance tolerance is not established"
                };
                let mut ev = evidence(r);
                if let Some(f) = r.acceptance.get("geometry_tolerance_mm") {
                    ev.extend(f.evidence())
                }
                report.record(&key, status, why, observed, ev)?;
                if let Some(f) = report.findings.iter_mut().find(|f| f.obligation.key == key) {
                    f.uncertainty_mm = Some(error)
                }
            }
            Err(why) => report.record(&key, Status::Incomplete, why, Value::Null, evidence(r))?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pdf(name: &str, layer: &str, extra: &str) -> Vec<u8> {
        pdf_after(name, layer, extra, "")
    }
    fn pdf_after(name: &str, layer: &str, extra: &str, after: &str) -> Vec<u8> {
        let stream = format!("/OC /L BDC /GS gs /Ink cs 1 scn {extra} 10 10 50 50 re f {after} EMC");
        pdf_stream(name, layer, &stream)
    }
    fn pdf_stream(name: &str, layer: &str, stream: &str) -> Vec<u8> {
        pdf_stream_alt(name, layer, stream, "0 1 0 0")
    }
    fn pdf_stream_alt(name: &str, layer: &str, stream: &str, alternate: &str) -> Vec<u8> {
        pdf_stream_options(name, layer, stream, alternate, "", 1)
    }
    fn pdf_stream_options(name: &str, layer: &str, stream: &str, alternate: &str, extra_space: &str, mode: u8) -> Vec<u8> {
        crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R] /D << /ON [6 0 R] >> >> >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /L 6 0 R >> /ColorSpace << /Ink [/Separation /{name} /DeviceCMYK 5 0 R] {extra_space} >> /ExtGState << /GS 7 0 R >> >> /Contents 4 0 R >>"
            ),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            format!("<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [{alternate}] /N 1 >>"),
            format!("<< /Type /OCG /Name ({layer}) >>"),
            format!("<< /Type /ExtGState /op true /OP true /OPM {mode} >>"),
        ])
    }
    fn request(bytes: &[u8]) -> Request {
        let mut r = crate::attribution::tests::request();
        r.baseline = None;
        r.geometry = None;
        r.identity.pdf_sha256 = crate::sha256(bytes);
        r.input_class = Some(crate::preparation::Fact {
            value: json!("individual_separation"),
            provenance: crate::preparation::Provenance::File {
                artifact_sha256: r.identity.pdf_sha256.clone(),
                locator: "Synthetic individual separation".into(),
            },
        });
        r.identity.assignments_sha256 = r.assignments_digest().unwrap();
        r
    }
    fn finding<'a>(r: &'a Report, key: &str) -> &'a crate::Finding {
        r.findings.iter().find(|f| f.obligation.key == key).unwrap()
    }
    #[test]
    fn independent_presence_and_routes_do_not_require_an_original() {
        for (name, layer, extra, route, presence) in [
            ("Exact#20ink", "Layer", "", Status::Pass, Status::Pass),
            ("Wrong", "Layer", "", Status::Fail, Status::Fail),
            ("Wrong", "Layer", "0 scn", Status::Fail, Status::Fail),
            ("Exact#20ink", "Wrong layer", "", Status::Fail, Status::Pass),
            ("Wrong", "Layer", "0 0 1 1 re W n", Status::Pass, Status::Fail),
        ] {
            let bytes = pdf(name, layer, extra);
            let r = request(&bytes);
            let out = crate::pdf::check(&r, &bytes).unwrap();
            assert_eq!(finding(&out, "observed/routing").status, route);
            assert_eq!(finding(&out, "observed/plate/dark-output/presence").status, presence);
            assert_eq!(finding(&out, "global/preparation").status, Status::Incomplete);
            assert_ne!(finding(&out, "plate/dark-output").status, Status::Pass);
        }
    }
    #[test]
    fn missing_scope_stale_assignment_and_unsupported_content_cannot_claim_observation() {
        for mode in 0..3 {
            let bytes = pdf("Exact#20ink", "Layer", if mode == 2 { "BI /W 1 /H 1 /BPC 8 /CS /G ID x EI" } else { "" });
            let mut r = request(&bytes);
            if mode == 0 {
                r.input_class = None
            }
            if mode == 1 {
                r.identity.assignments_sha256 = "stale".into()
            }
            let out = crate::pdf::check(&r, &bytes).unwrap();
            assert_eq!(finding(&out, "observed/routing").status, Status::Incomplete);
        }
    }
    #[test]
    fn shared_plate_only_anchors_the_complete_unfiltered_combination() {
        let bytes = pdf("Exact#20ink", "Layer", "");
        let mut r = request(&bytes);
        let imported = vectorcraft_pdf::import(&bytes).unwrap();
        let mut c = crate::pdf::census(&imported);
        crate::ordering::reconcile(&bytes, &mut c);
        let mut other = r.recipe.rules[0].clone();
        other.rule_key = "other-component".into();
        r.recipe.rules.push(other);
        let mut link = r.recipe.file_links[0].clone();
        link.key = "other-output".into();
        link.source_rule = "other-component".into();
        r.recipe.file_links.push(link);
        let mut combined = r.recipe.rules[0].clone();
        combined.rule_key = "whole-plate".into();
        combined.operation = "combine".into();
        combined.distance_mm = None;
        combined.sources = vec![
            crate::recipe::Source { kind: "rule".into(), key: "arbitrary".into() },
            crate::recipe::Source { kind: "rule".into(), key: "other-component".into() },
        ];
        r.recipe.rules.push(combined);
        let f = r.recipe.foundation.as_mut().unwrap();
        f["treatments"]["whole-plate"] = f["treatments"]["arbitrary"].clone();
        f["outputs"]["other-output"] = f["outputs"]["output"].clone();
        assert!(anchor(&r, &c, &Survival::new(&c), "arbitrary").is_err());
        assert_eq!(anchor(&r, &c, &Survival::new(&c), "whole-plate").unwrap().len(), 1);
        r.recipe.rules.last_mut().unwrap().sources.pop();
        assert!(anchor(&r, &c, &Survival::new(&c), "whole-plate").is_err());
        r.recipe.rules.last_mut().unwrap().sources.push(crate::recipe::Source { kind: "rule".into(), key: "other-component".into() });
        r.recipe.foundation.as_mut().unwrap()["treatments"]["whole-plate"]["select"] =
            json!([[{"field":"role","operator":"equals","value":"ordinary"}]]);
        assert!(anchor(&r, &c, &Survival::new(&c), "whole-plate").is_err());
    }
    #[test]
    fn required_absent_contribution_fails_and_single_observed_order_is_decidable() {
        let bytes = pdf("Exact#20ink", "Layer", "");
        let r = request(&bytes);
        let out = crate::pdf::check(&r, &bytes).unwrap();
        assert_eq!(finding(&out, "observed/file/output/order").status, Status::Pass);
        let bytes = pdf("Exact#20ink", "Layer", "0 0 1 1 re W n");
        let mut r = request(&bytes);
        r.recipe.foundation.as_mut().unwrap()["verification"]["contributions"]["output"]["emptiness"] = json!("required");
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        let out = crate::pdf::check(&r, &bytes).unwrap();
        assert_eq!(finding(&out, "observed/file/output/paint").status, Status::Fail);
    }
    #[test]
    fn shared_source_route_cannot_become_a_technical_anchor() {
        let bytes = pdf("Exact#20ink", "Layer", "");
        let mut r = request(&bytes);
        let imported = vectorcraft_pdf::import(&bytes).unwrap();
        let mut c = crate::pdf::census(&imported);
        crate::ordering::reconcile(&bytes, &mut c);
        let mut other = r.recipe.file_links[0].clone();
        other.key = "retained-source".into();
        other.name_mode = "source".into();
        other.separation_name = None;
        other.source_rule = "other-rule".into();
        r.recipe.file_links.push(other);
        assert!(anchor(&r, &c, &Survival::new(&c), "arbitrary").is_err());
    }
    #[test]
    fn later_zero_tint_cannot_claim_a_surviving_plate() {
        let bytes = pdf_after("Exact#20ink", "Layer", "", "0 scn 0 0 100 100 re f");
        let r = request(&bytes);
        let out = crate::pdf::check(&r, &bytes).unwrap();
        assert_eq!(finding(&out, "observed/plate/dark-output/presence").status, Status::Incomplete);
    }
    #[test]
    fn same_technical_ink_on_another_layer_cannot_anchor_a_partial_plate() {
        let bytes = pdf("Exact#20ink", "Layer", "");
        let r = request(&bytes);
        let imported = vectorcraft_pdf::import(&bytes).unwrap();
        let mut c = crate::pdf::census(&imported);
        crate::ordering::reconcile(&bytes, &mut c);
        assert!(anchor(&r, &c, &Survival::new(&c), "arbitrary").is_ok());
        let mut foreign = c.paints[0].clone();
        foreign.layer = "Other layer".into();
        foreign.id += 1;
        c.paints.push(foreign);
        assert!(anchor(&r, &c, &Survival::new(&c), "arbitrary").is_err());
    }
    #[test]
    fn white_process_paint_and_later_process_overwrite_cannot_prove_indigo_ink() {
        for (paint, after, expected) in [
            ("0 g", "", Status::Pass),
            ("1 g", "", Status::Incomplete),
            ("0 0 0 0 k", "", Status::Incomplete),
            ("0 1 0 0 k", "", Status::Pass),
            ("0 1 0 0 k", "0 0 0 0 k 0 0 100 100 re f", Status::Incomplete),
        ] {
            let bytes = pdf_after("unused", "Layer", paint, after);
            let mut r = request(&bytes);
            r.recipe.file_links[0].name_mode = "source".into();
            r.recipe.file_links[0].separation_name = None;
            r.recipe.file_links[0].destination = "Indigo".into();
            r.recipe.required_screens = Some(vec![]);
            let f = r.recipe.foundation.as_mut().unwrap();
            f["treatments"]["arbitrary"]["paint"] = json!({"mode":"source"});
            for p in f["verification"]["plates"].as_array_mut().unwrap() {
                p["name"] = Value::Null;
                p["name_mode"] = json!("composite");
                p["destination"] = json!("Indigo");
                p["required"] = json!(false)
            }
            r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
            let out = crate::pdf::check(&r, &bytes).unwrap();
            assert_eq!(finding(&out, "observed/plate/dark-output/presence").status, expected);
        }
    }
    #[test]
    fn zero_width_filled_path_cannot_prove_physical_presence() {
        let bytes = pdf_stream("Exact#20ink", "Layer", "/OC /L BDC /GS gs /Ink cs 1 scn 10 10 0 50 re f EMC");
        let r = request(&bytes);
        let out = crate::pdf::check(&r, &bytes).unwrap();
        assert_ne!(
            finding(&out, "observed/plate/dark-output/presence").status,
            Status::Pass,
            "{:?}",
            crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap())
        );
    }
    #[test]
    fn composite_white_alternate_and_reserved_colourants_cannot_prove_ink() {
        for (name, alternate, expected) in
            [("Custom", "0 1 0 0", true), ("Custom", "0 0 0 0", false), ("None", "0 1 0 0", false), ("All", "0 1 0 0", false)]
        {
            let bytes = pdf_stream_alt(name, "Layer", "/OC /L BDC /GS gs /Ink cs 1 scn 10 10 50 50 re f EMC", alternate);
            let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
            crate::ordering::reconcile(&bytes, &mut c);
            assert_eq!(c.paints.iter().any(|p| positive_ink(&c, p, true)), expected, "{name}: {alternate}");
            if matches!(name, "None" | "All") {
                assert!(!c.paint_order_verified);
            }
        }
    }
    #[test]
    fn source_layer_custom_inks_are_allowed_without_claiming_colour_fidelity() {
        let bytes = pdf("A#20custom#20ink", "Layer", "");
        let mut r = request(&bytes);
        r.recipe.file_links[0].name_mode = "source".into();
        r.recipe.file_links[0].separation_name = None;
        r.recipe.file_links[0].destination = "Indigo".into();
        r.recipe.required_screens = Some(vec![]);
        let f = r.recipe.foundation.as_mut().unwrap();
        f["treatments"]["arbitrary"]["paint"] = json!({"mode":"source"});
        for p in f["verification"]["plates"].as_array_mut().unwrap() {
            p["name"] = Value::Null;
            p["name_mode"] = json!("composite");
            p["destination"] = json!("Indigo");
            p["required"] = json!(false)
        }
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        assert!(crate::contract::issues(&r.recipe).is_empty());
        let out = crate::pdf::check(&r, &bytes).unwrap();
        assert_eq!(finding(&out, "observed/routing").status, Status::Pass);
        assert_eq!(finding(&out, "observed/file/output/paint").status, Status::Pass);
        assert_ne!(finding(&out, "global/colour").status, Status::Pass);
    }
    fn composite_request(bytes: &[u8]) -> Request {
        let mut r = request(bytes);
        r.recipe.file_links[0].name_mode = "source".into();
        r.recipe.file_links[0].separation_name = None;
        r.recipe.file_links[0].destination = "Indigo".into();
        r.recipe.required_screens = Some(vec![]);
        let f = r.recipe.foundation.as_mut().unwrap();
        f["treatments"]["arbitrary"]["paint"] = json!({"mode":"source"});
        for p in f["verification"]["plates"].as_array_mut().unwrap() {
            p["name"] = Value::Null;
            p["name_mode"] = json!("composite");
            p["destination"] = json!("Indigo");
            p["required"] = json!(false);
        }
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        r
    }
    #[test]
    fn direct_process_separation_presence_uses_only_its_named_component() {
        for (name, channel) in [("Cyan", "c"), ("Magenta", "m"), ("Yellow", "y"), ("Black", "k")] {
            for tint in [0.0, 0.25, 1.0] {
                // A paper-white alternate must not replace an available direct
                // subtractive process colourant. The fixture is independently authored.
                let bytes = pdf_stream_alt(name, "Layer", &format!("/OC /L BDC /GS gs /Ink cs {tint} scn 10 10 50 50 re f EMC"), "0 0 0 0");
                let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
                crate::ordering::reconcile(&bytes, &mut c);
                assert!(c.paint_order_verified, "{name}/{tint}: {:?}", c.unresolved);
                assert_eq!(c.paints.len(), 1);
                let p = &c.paints[0];
                assert!(p.ink.is_none());
                assert_eq!(p.paint["color"][channel], json!(tint));
                assert_eq!(positive_ink(&c, p, true), tint > 0.0);
                let out = crate::pdf::check(&composite_request(&bytes), &bytes).unwrap();
                assert_eq!(finding(&out, "observed/plate/dark-output/presence").status, if tint > 0.0 { Status::Pass } else { Status::Incomplete });
                let mut wrong = p.clone();
                for key in ["c", "m", "y", "k"] {
                    wrong.paint["color"][key] = json!(if key == channel { 0.0 } else { 1.0 });
                }
                assert!(!positive_ink(&c, &wrong, true), "An unnamed channel cannot prove {name}");
                wrong.paint["color"][channel] = json!(2.0);
                assert!(!positive_ink(&c, &wrong, true));
                let index = p.source_space.unwrap();
                let mut unknown = c.clone();
                unknown.paint_sources[index].colorants = vec!["Unqualified".into()];
                assert!(!positive_ink(&unknown, p, true));
            }
            let bytes = pdf_stream_alt(name, "Layer", "/OC /L BDC /GS gs /Ink cs 1 scn 10 10 50 50 re f 0 scn 0 0 100 100 re f EMC", "0 0 0 0");
            let out = crate::pdf::check(&composite_request(&bytes), &bytes).unwrap();
            assert_eq!(finding(&out, "observed/plate/dark-output/presence").status, Status::Incomplete, "Later zero process paint: {name}");
        }
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn independent_direct_process_separation_planes_match_positive_and_zero_tints() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else { return };
        let binary_sha256 = crate::sha256(&std::fs::read(&binary).unwrap());
        for name in ["Cyan", "Magenta", "Yellow", "Black"] {
            for tint in [0.0, 0.5] {
                for dpi in [72, 144] {
                    for mode in [0, 1] {
                        let bytes = pdf_stream_alt(name, "Layer", &format!("/OC /L BDC /GS gs /Ink cs {tint} scn 10 10 50 50 re f EMC"), "0 0 0 0");
                        let bytes = if mode == 0 { String::from_utf8(bytes).unwrap().replace("/OPM 1", "/OPM 0").into_bytes() } else { bytes };
                        let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
                        crate::ordering::reconcile(&bytes, &mut c);
                        assert!(c.paint_order_verified);
                        let output = std::env::temp_dir().join(format!(
                            "vc-direct-process-{name}-{tint}-{dpi}-{mode}-{}",
                            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
                        ));
                        let config = crate::independent::Configuration {
                            binary: binary.clone().into(),
                            binary_sha256: binary_sha256.clone(),
                            version: "10.07.1".into(),
                            dpi,
                            timeout_ms: 10_000,
                            max_output_bytes: 16 * 1024 * 1024,
                            qualification_sha256: None,
                        };
                        let render = crate::independent::render(&config, &bytes, &output).unwrap();
                        let comparison = crate::raster::compare(&c, &render, Some(0.6)).unwrap();
                        assert_eq!(comparison.status, Status::Pass, "{name}/{tint}/{dpi}/{mode}: {comparison:?}");
                        assert_eq!(c.paints.iter().any(|p| positive_ink(&c, p, true)), tint > 0.0);
                        assert!(!render.qualified);
                        std::fs::remove_dir_all(output).unwrap();
                    }
                }
            }
        }
    }
    #[test]
    fn certified_gap_witness_keeps_boundaries_and_cannot_pass_morphology() {
        fn rect(x: f64, y: f64, w: f64, h: f64) -> vectorcraft_geom::PathData {
            let mut p = vectorcraft_geom::BezPath::new();
            p.move_to((x, y));
            p.line_to((x + w, y));
            p.line_to((x + w, y + h));
            p.line_to((x, y + h));
            p.close_path();
            vectorcraft_geom::PathData::from_bezpath(&p)
        }
        let source = rect(0.0, 0.0, 10.0, 10.0);
        // A 1 mm choke displaced 0.2 mm has gaps ranging from 0.8 to 1.2 mm.
        let target = rect(1.2, 1.0, 8.0, 8.0);
        let gap = crate::geometry::boundary_gap(&target, &source, 0.0001).unwrap();
        assert!(gap.minimum_mm[0] <= 0.8 && gap.minimum_mm[1] >= 0.8);
        assert!(gap.maximum_mm[0] <= 1.2 && gap.maximum_mm[1] >= 1.2);
        let (lower, status) = gap_witness(&gap, 1.0, Some(0.19));
        assert!((0.199..=0.2).contains(&lower));
        assert_eq!(status, Status::Fail);
        for tolerance in [None, Some(lower), Some(0.2), Some(0.21)] {
            assert_eq!(gap_witness(&gap, 1.0, tolerance).1, Status::Incomplete);
        }
        assert_eq!(gap_witness(&gap, 1.0, Some(lower - 0.000001)).1, Status::Fail);
        let exact = crate::geometry::boundary_gap(&rect(1.0, 1.0, 8.0, 8.0), &source, 0.0001).unwrap();
        assert_eq!(gap_witness(&exact, 1.0, Some(0.01)), (0.0, Status::Incomplete));
        // Distance-only evidence has no inward/outward orientation and must not
        // Pass a purported spread merely because the inward choke gaps match.
        assert_eq!(gap_witness(&exact, 1.0, Some(10.0)).1, Status::Incomplete);
        let mut wide = gap;
        wide.minimum_mm[1] = 1.0;
        wide.maximum_mm[0] = 1.0;
        assert_eq!(gap_witness(&wide, 1.0, Some(0.0)), (0.0, Status::Incomplete));
    }
    #[test]
    fn later_process_zero_cannot_leave_composite_spot_alternate_presence_passing() {
        for (after, mode, expected) in [
            ("", 1, Status::Pass),
            ("/Zero cs 0 scn 0 0 100 100 re f", 1, Status::Incomplete),
            ("0 0 0 0 k 0 0 100 100 re f", 0, Status::Incomplete),
            // Conservative when a later OPM1 zero might retain the earlier ink.
            ("0 0 0 0 k 0 0 100 100 re f", 1, Status::Incomplete),
            ("0 0 0 1 k 0 0 100 100 re f", 0, Status::Pass),
        ] {
            let bytes = pdf_stream_options(
                "Exact#20ink",
                "Layer",
                &format!("/OC /L BDC /GS gs /Ink cs 1 scn 10 10 50 50 re f {after} EMC"),
                "1 0 0 0",
                "/Zero [/Separation /Cyan /DeviceCMYK 5 0 R]",
                mode,
            );
            let out = crate::pdf::check(&composite_request(&bytes), &bytes).unwrap();
            assert_eq!(finding(&out, "observed/plate/dark-output/presence").status, expected, "{after}/{mode}");
            // Actual named technical ink remains independent of its CMYK alternate.
            let out = crate::pdf::check(&request(&bytes), &bytes).unwrap();
            assert_eq!(finding(&out, "observed/plate/dark-output/presence").status, Status::Pass, "Technical spot {after}/{mode}");
        }
    } // Original public-syntax image + technical-vector fixtures. No customer data.
    fn prelude_pdf(mode: &str) -> Vec<u8> {
        let image = if mode == "clipped" {
            "/OC /S BDC q 0 0 1 1 re W n 10 0 0 10 30 30 cm /Im Do Q EMC"
        } else {
            "/OC /S BDC q 10 0 0 10 30 30 cm /Im Do Q EMC"
        };
        let vector = match mode {
            "wrong-route" => "/OC /W BDC /GS gs /Ink cs 1 scn 10 10 50 50 re f EMC",
            "wrong-flag" => "/OC /L BDC /Ink cs 1 scn 10 10 50 50 re f EMC",
            "no-vector" => "",
            _ => "/OC /L BDC /GS gs /Ink cs 1 scn 10 10 50 50 re f EMC",
        };
        let stream = match mode {
            "missing-state" => format!("/Missing gs {image} {vector}"),
            "after" => format!("{vector} {image}"),
            "balanced-form-owned" => format!(
                "{image} /OC /W BDC /Fm Do EMC /OC /L BDC /KO gs /Ink cs 0 scn 10 10 50 50 re f EMC /OC /W BDC /Fm Do EMC /OC /L BDC /Fa Do /Fa Do EMC"
            ),
            "used-source-duplicate" => format!("{image} /OC /W BDC q 10 0 0 10 30 30 cm /Im Do Q EMC {vector}"),
            "used-technical-duplicate" => format!("{image} {vector} /OC /W BDC /GS gs /Ink cs 1 scn 70 70 5 5 re f EMC"),
            "form-owned-duplicate" => format!("{image} /OC /W BDC /Fm Do EMC {vector} /OC /W BDC /Fm Do EMC"),
            "empty-scope-duplicate" => format!("/OC /W BDC EMC {image} {vector}"),
            "clipped-duplicate" => format!("{image} {vector} /OC /W BDC q 0 0 1 1 re W n /GS gs /Ink cs 1 scn 70 70 5 5 re f Q EMC"),
            "zero-tint-duplicate" => format!("{image} {vector} /OC /W BDC /GS gs /Ink cs 0 scn 10 10 50 50 re f EMC"),
            _ => format!("{image} {vector}"),
        };
        let extras = match mode {
            "group" => "/Group << /S /Transparency /CS /DeviceCMYK >>",
            _ => "",
        };
        let image_extras = match mode {
            "intent-relative" => "/Intent /RelativeColorimetric",
            "intent-absolute" => "/Intent /AbsoluteColorimetric",
            "intent-perceptual" => "/Intent /Perceptual",
            "intent-saturation" => "/Intent /Saturation",
            "interpolate" => "/Interpolate true",
            "alpha-zero" | "alpha-positive" => "/SMask 10 0 R",
            _ => "",
        };
        let space = if mode == "technical-image" { "[/Separation /Exact#20ink /DeviceCMYK 5 0 R]" } else { "/DeviceGray" };
        let layer = if mode == "duplicate" { "Layer" } else { "Source" };
        let unused_layer = match mode {
            "unused-source-duplicate" | "used-source-duplicate" => "Source",
            "unused-technical-duplicate"
            | "used-technical-duplicate"
            | "form-owned-duplicate"
            | "empty-scope-duplicate"
            | "clipped-duplicate"
            | "zero-tint-duplicate"
            | "two-page-duplicate" => "Layer",
            "synthetic-name" => "Layer 3",
            "balanced-form-owned" => "Second",
            _ => "Unrouted",
        };
        let alpha = if mode == "alpha-zero" { "00>" } else { "FF>" };
        crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [11 0 R 6 0 R 9 0 R] /D << /ON [11 0 R 6 0 R 9 0 R] >> >> >>".into(),
            if mode == "two-page-duplicate" { "<< /Type /Pages /Kids [3 0 R 13 0 R] /Count 2 >>" } else { "<< /Type /Pages /Kids [3 0 R] /Count 1 >>" }.into(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] {extras} /Resources << /Properties << /L 6 0 R /S 9 0 R /W 11 0 R >> /XObject << /Im 8 0 R /Fm 12 0 R /Fa 15 0 R >> /ColorSpace << /Ink [/Separation /Exact#20ink /DeviceCMYK 5 0 R] >> /ExtGState << /GS 7 0 R /KO 16 0 R >> >> /Contents 4 0 R >>"
            ),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>".into(),
            "<< /Type /OCG /Name (Layer) >>".into(),
            "<< /Type /ExtGState /op true /OP true /OPM 1 >>".into(),
            format!(
                "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace {space} {image_extras} /Filter /ASCIIHexDecode /Length 3 >>\nstream\n80>\nendstream"
            ),
            if mode == "synthetic-name" { "<< /Type /OCG >>".into() } else { format!("<< /Type /OCG /Name ({layer}) >>") },
            format!(
                "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode /Length 3 >>\nstream\n{alpha}\nendstream"
            ),
            format!("<< /Type /OCG /Name ({unused_layer}) >>"),
            {
                let content = if mode == "balanced-form-owned" { "/GS gs /Ink cs 1 scn 10 10 50 50 re f" } else { "/GS gs /Ink cs 0 scn 10 10 50 50 re f" };
                format!("<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /OC 6 0 R /Length {} >>\nstream\n{content}\nendstream", content.len())
            },
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /W 11 0 R >> /ColorSpace << /Ink [/Separation /Exact#20ink /DeviceCMYK 5 0 R] >> /ExtGState << /GS 7 0 R >> >> /Contents 14 0 R >>".into(),
            {
                let content = "/OC /W BDC /GS gs /Ink cs 1 scn 10 10 50 50 re f EMC";
                format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len())
            },
            {
                let content = "/KO gs /Ink cs 0 scn 10 10 50 50 re f";
                format!("<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /OC 11 0 R /Length {} >>\nstream\n{content}\nendstream", content.len())
            },
            "<< /Type /ExtGState /op false /OP false /OPM 0 >>".into(),
        ])
    }
    fn prelude_request(bytes: &[u8]) -> Request {
        let mut r = request(bytes);
        let mut rule = r.recipe.rules[0].clone();
        rule.rule_key = "image-source".into();
        r.recipe.rules.push(rule);
        let mut link = r.recipe.file_links[0].clone();
        link.key = "image-output".into();
        link.source_rule = "image-source".into();
        link.name_mode = "source".into();
        link.separation_name = None;
        link.pdf_layer = "Source".into();
        if vectorcraft_pdf::print_order(bytes).unwrap().issues.iter().any(|s| s.contains("Synthesized PDF layer names")) {
            link.pdf_layer = "Layer 3".into();
        }
        link.destination = "Indigo".into();
        link.layer_order = 0;
        r.recipe.file_links[0].layer_order = 1;
        r.recipe.file_links.push(link);
        let f = r.recipe.foundation.as_mut().unwrap();
        f["treatments"]["image-source"] = f["treatments"]["arbitrary"].clone();
        f["treatments"]["image-source"]["paint"] = json!({"mode":"source"});
        f["outputs"]["image-output"] = f["outputs"]["output"].clone();
        f["verification"]["contributions"]["image-output"] = json!({"emptiness":"required"});
        for garment in ["dark", "light"] {
            f["verification"]["plates"].as_array_mut().unwrap().push(json!({"key":format!("{garment}-image"),"garment":garment,"name":null,"name_mode":"composite","destination":"Indigo","required":false,"links":["image-output"],"evidence":"synthetic specification"}));
        }
        if vectorcraft_pdf::print_order(bytes).unwrap().layers.iter().any(|n| n == "Second") {
            let mut rule = r.recipe.rules[0].clone();
            let old_rule = rule.rule_key.clone();
            rule.rule_key = "second".into();
            r.recipe.rules.push(rule);
            let mut link = r.recipe.file_links[0].clone();
            let old_link = link.key.clone();
            link.key = "second".into();
            link.source_rule = "second".into();
            link.pdf_layer = "Second".into();
            r.recipe.file_links.push(link);
            f["treatments"]["second"] = f["treatments"][&old_rule].clone();
            f["outputs"]["second"] = f["outputs"][&old_link].clone();
            f["verification"]["contributions"]["second"] = f["verification"]["contributions"][&old_link].clone();
            for plate in f["verification"]["plates"].as_array_mut().unwrap() {
                if plate["links"].as_array().unwrap().contains(&json!(old_link)) {
                    plate["links"].as_array_mut().unwrap().push(json!("second"));
                }
            }
        }
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        assert!(crate::contract::issues(&r.recipe).is_empty(), "{:?}", crate::contract::issues(&r.recipe));
        r
    }
    #[test]
    fn process_image_prelude_isolates_technical_vectors_without_claiming_image_paint() {
        for mode in ["plain", "alpha-zero", "alpha-positive", "clipped"] {
            let bytes = prelude_pdf(mode);
            let r = prelude_request(&bytes);
            let out = crate::pdf::check(&r, &bytes).unwrap();
            assert_eq!(
                finding(&out, "observed/plate/dark-output/presence").status,
                Status::Pass,
                "{mode}: {:?}",
                finding(&out, "observed/plate/dark-output/presence")
            );
            assert_eq!(finding(&out, "observed/file/output/paint").status, Status::Pass);
            for key in [
                "observed/routing",
                "observed/file/output/order",
                "observed/file/image-output/order",
                "observed/file/image-output/paint",
                "observed/plate/dark-image/presence",
                "global/integrity",
            ] {
                assert_eq!(finding(&out, key).status, Status::Incomplete, "{mode}/{key}");
            }
            assert_eq!(finding(&out, "global/integrity").observed["raw_paint_audit"]["image_use_count"], json!(1));
            assert_eq!(out.findings.len(), crate::enumerate(&r).unwrap().len());
        }
    }
    #[test]
    fn technical_vector_defects_and_absence_remain_decidable_after_process_image() {
        for (mode, key) in
            [("wrong-flag", "observed/file/output/paint"), ("wrong-route", "observed/routing"), ("no-vector", "observed/plate/dark-output/presence")]
        {
            let bytes = prelude_pdf(mode);
            let r = prelude_request(&bytes);
            let out = crate::pdf::check(&r, &bytes).unwrap();
            assert_eq!(finding(&out, key).status, Status::Fail, "{mode}: {:?}", finding(&out, key));
            assert_eq!(finding(&out, "observed/file/image-output/paint").status, Status::Incomplete);
        }
    }
    #[test]
    fn image_prelude_refuses_later_images_unknown_context_and_technical_ink() {
        for mode in [
            "missing-state",
            "intent-relative",
            "intent-absolute",
            "intent-perceptual",
            "intent-saturation",
            "after",
            "group",
            "interpolate",
            "technical-image",
            "duplicate",
            "used-source-duplicate",
            "used-technical-duplicate",
            "clipped-duplicate",
            "zero-tint-duplicate",
            "two-page-duplicate",
            "synthetic-name",
        ] {
            let bytes = prelude_pdf(mode);
            let r = prelude_request(&bytes);
            let out = crate::pdf::check(&r, &bytes).unwrap();
            assert!(out.findings.iter().filter(|f| f.obligation.key.starts_with("observed/")).all(|f| f.status == Status::Incomplete), "{mode}");
            if mode.contains("duplicate") {
                assert!(finding(&out, "observed/routing").reason.contains("Multiple painted layer identities"), "{mode}");
            }
            if mode == "synthetic-name" {
                assert!(vectorcraft_pdf::print_order(&bytes).unwrap().issues.iter().any(|s| s.contains("Synthesized PDF layer names")));
            }
        }
        let bytes = prelude_pdf("plain");
        let mut r = prelude_request(&bytes);

        // Exercise the proof directly: even exact process names must not be
        // considered absent merely because they are carried by an image.
        let imported = vectorcraft_pdf::import_with_report(&bytes, &Default::default()).unwrap();
        let mut c = crate::pdf::census(&imported.document);
        let _ = crate::ordering::try_reconcile(&bytes, &mut c);
        for name in ["Cyan", "Magenta", "Yellow", "Black", "All", "None"] {
            r.recipe.file_links[0].separation_name = Some(name.into());
            assert!(crate::observed_images::Prelude::new(&r, &bytes, &imported.document, &imported.warnings, &c).is_err(), "{name}");
        }
    }
    #[test]
    fn image_isolation_rejects_mismatched_inventories_frames_routes_and_import_loss() {
        let bytes = prelude_pdf("plain");
        let r = prelude_request(&bytes);
        let imported = vectorcraft_pdf::import_with_report(&bytes, &Default::default()).unwrap();
        let mut general = crate::pdf::census(&imported.document);
        let _ = crate::ordering::try_reconcile(&bytes, &mut general);
        assert!(crate::observed_images::Prelude::new(&r, &bytes, &imported.document, &imported.warnings, &general).is_ok());
        general.raw_audit.as_mut().unwrap().image_use_count += 1;
        assert!(crate::observed_images::Prelude::new(&r, &bytes, &imported.document, &imported.warnings, &general).is_err());
        general.raw_audit.as_mut().unwrap().image_use_count -= 1;
        assert!(crate::observed_images::Prelude::new(&r, &bytes, &imported.document, &["Unknown import loss".into()], &general).is_err());
        let mut changed = imported.document.clone();
        changed.artboards[0].rect.x1 += 1.0;
        assert!(crate::observed_images::Prelude::new(&r, &bytes, &changed, &imported.warnings, &general).is_err());
        let mut changed = r.clone();
        changed.recipe.file_links[1].pdf_layer = "Unknown route".into();
        assert!(crate::observed_images::Prelude::new(&changed, &bytes, &imported.document, &imported.warnings, &general).is_err());
        changed = r.clone();
        changed.recipe.file_links.push(changed.recipe.file_links[1].clone());
        assert!(crate::observed_images::Prelude::new(&changed, &bytes, &imported.document, &imported.warnings, &general).is_err());
    }
    #[test]
    fn unused_duplicate_declarations_do_not_obscure_unique_painted_identities() {
        for mode in ["unused-source-duplicate", "unused-technical-duplicate", "empty-scope-duplicate"] {
            let bytes = prelude_pdf(mode);
            let out = crate::pdf::check(&prelude_request(&bytes), &bytes).unwrap();
            assert_eq!(finding(&out, "observed/plate/dark-output/presence").status, Status::Pass, "{mode}");
            assert_eq!(finding(&out, "observed/file/output/paint").status, Status::Pass, "{mode}");
            assert_eq!(finding(&out, "observed/file/image-output/paint").status, Status::Incomplete);
            assert_eq!(finding(&out, "global/integrity").status, Status::Incomplete);
            assert_eq!(finding(&out, "observed/routing").observed["image_prelude"]["unused_duplicate_layer_declarations"], json!(1));
            assert!(
                finding(&out, "global/integrity").observed["raw_paint_audit"]["issues_sample"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s.as_str().is_some_and(|s| s.contains("Duplicate PDF layer")))
            );
        }
    }
    #[test]
    fn form_owned_oc_cannot_merge_divergent_native_layer_attribution() {
        let bytes = prelude_pdf("form-owned-duplicate");
        let out = crate::pdf::check(&prelude_request(&bytes), &bytes).unwrap();
        assert!(
            out.findings.iter().filter(|f| f.obligation.key.starts_with("observed/")).all(|f| f.status == Status::Incomplete),
            "{:?}",
            finding(&out, "observed/plate/dark-output/presence")
        );
        assert!(finding(&out, "observed/routing").reason.contains("Multiple native painted layer identities"));
    }
    #[test]
    fn balanced_form_oc_counts_cannot_certify_reordered_plate_presence() {
        let bytes = prelude_pdf("balanced-form-owned");
        let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
        assert_eq!(raw.layers.iter().collect::<BTreeSet<_>>().len(), raw.layers.len());
        assert!(raw.issues.iter().any(|s| s.contains("Form-owned optional content")));
        let out = crate::pdf::check(&prelude_request(&bytes), &bytes).unwrap();
        assert!(out.findings.iter().filter(|f| f.obligation.key.starts_with("observed/")).all(|f| f.status == Status::Incomplete));
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn image_prelude_exported_cases_have_independent_expected_statuses() {
        let Ok(dir) = std::env::var("CHECK_OBSERVED_IMAGE_EVIDENCE_DIR") else { return };
        let dir = std::path::Path::new(&dir);
        std::fs::create_dir_all(dir).unwrap();
        let mut cases = vec![];
        for mode in [
            "plain",
            "alpha-zero",
            "alpha-positive",
            "clipped",
            "wrong-flag",
            "wrong-route",
            "no-vector",
            "after",
            "group",
            "interpolate",
            "technical-image",
            "duplicate",
            "unused-source-duplicate",
            "unused-technical-duplicate",
            "used-source-duplicate",
            "used-technical-duplicate",
            "form-owned-duplicate",
            "empty-scope-duplicate",
            "clipped-duplicate",
            "zero-tint-duplicate",
            "two-page-duplicate",
            "synthetic-name",
            "balanced-form-owned",
        ] {
            let bytes = prelude_pdf(mode);
            let r = prelude_request(&bytes);
            let out = crate::pdf::check(&r, &bytes).unwrap();
            let mut expected = serde_json::Map::new();
            let refused = [
                "after",
                "group",
                "interpolate",
                "technical-image",
                "duplicate",
                "used-source-duplicate",
                "used-technical-duplicate",
                "form-owned-duplicate",
                "clipped-duplicate",
                "zero-tint-duplicate",
                "two-page-duplicate",
                "synthetic-name",
                "balanced-form-owned",
            ]
            .contains(&mode);
            expected.insert("observed/file/image-output/paint".into(), json!("incomplete"));
            expected.insert("observed/file/output/order".into(), json!("incomplete"));
            expected.insert("observed/plate/dark-image/presence".into(), json!("incomplete"));
            expected.insert(
                "observed/plate/dark-output/presence".into(),
                json!(if refused {
                    "incomplete"
                } else if mode == "no-vector" {
                    "fail"
                } else {
                    "pass"
                }),
            );
            expected.insert("observed/routing".into(), json!(if mode == "wrong-route" { "fail" } else { "incomplete" }));
            if !refused && mode != "wrong-route" && mode != "no-vector" {
                expected.insert("observed/file/output/paint".into(), json!(if mode == "wrong-flag" { "fail" } else { "pass" }));
            }
            for (key, status) in &expected {
                assert_eq!(serde_json::to_value(finding(&out, key).status).unwrap(), *status, "{mode}/{key}");
            }
            std::fs::write(dir.join(format!("{mode}.pdf")), bytes).unwrap();
            std::fs::write(dir.join(format!("{mode}.json")), serde_json::to_vec(&r).unwrap()).unwrap();
            cases.push(json!({"name":mode,"args":[format!("/case/{mode}.json"),format!("/case/{mode}.pdf")],"expected":expected,"expected_image_audit_count":if mode=="used-source-duplicate" {2} else {1}}));
        }
        std::fs::write(dir.join("parity-cases.json"), serde_json::to_vec_pretty(&cases).unwrap()).unwrap();
    }
}
