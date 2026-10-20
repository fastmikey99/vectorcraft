//! Practical colour-trap observations. Explicit intent never changes screen spreads.
use crate::{
    Evidence, Obligation, Owner, Report, Request, Status,
    pdf::{Census, PaintedObject},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use vectorcraft_doc::trapping::{TrapMode, TrappingIntent};
use vectorcraft_geom::{FillRule, PathData, Rect, Shape};
use vectorcraft_pathops::BoolOp;

pub fn intent(r: &Request) -> Result<Option<TrappingIntent>, String> {
    r.acceptance
        .get("trapping_intent")
        .filter(|f| !f.value.is_null())
        .map(|f| {
            let i: TrappingIntent = serde_json::from_value(f.value.clone()).map_err(|e| e.to_string())?;
            i.validate()?;
            Ok(i)
        })
        .transpose()
}
fn colour_rules(r: &Request) -> Vec<&crate::recipe::Rule> {
    r.recipe
        .rules
        .iter()
        .filter(|rule| {
            r.garment.matches(&rule.garment)
                && rule.operation == "spread"
                && r.recipe
                    .foundation
                    .as_ref()
                    .and_then(|f| f.get("treatments"))
                    .and_then(|t| t.get(&rule.rule_key))
                    .is_some_and(|t| t.get("result") == Some(&json!("outer_band")) && t.pointer("/paint/mode") == Some(&json!("source")))
                && r.recipe.file_links.iter().any(|l| l.source_rule == rule.rule_key && l.destination == "Indigo" && r.garment.matches(&l.garment))
        })
        .collect()
}
pub(crate) fn enumerate(r: &Request, out: &mut Vec<Obligation>) {
    out.push(Obligation {
        key: "print/trapping-intent".into(),
        area: "Print quality".into(),
        label: "Trapping choice and known No trap regions".into(),
        expected: r.acceptance.get("trapping_intent").map(|f| f.value.clone()).unwrap_or(Value::Null),
        rule: None,
        file_link: None,
        plate: None,
        owner: Owner::Operator,
        conditional: false,
    });
}
fn record(r: &Request, report: &mut Report, key: &str, status: Status, reason: &str, observed: Value) -> Result<(), String> {
    report.record(
        key,
        status,
        reason,
        observed,
        vec![Evidence {
            artifact_sha256: r.identity.pdf_sha256.clone(),
            producer: "checks/colour-trapping-1".into(),
            detail: "Bounded final-sep vector colour-trap observation; no ticket or temporary LIGHT NO BLEED layer required".into(),
        }],
    )
}
fn boolean(a: &PathData, b: &PathData, op: BoolOp) -> Result<PathData, String> {
    vectorcraft_pathops::try_boolean_for_measurement(a, FillRule::NonZero, b, FillRule::NonZero, op).map_err(|e| e.to_string())
}
fn positive(p: &PaintedObject) -> bool {
    if p.ink.as_deref() == Some("None") || p.tint == Some(0.0) {
        return false;
    }
    if p.ink.is_some() {
        return true;
    }
    let color = p.paint.get("color");
    match color.and_then(|c| c.get("model")).and_then(Value::as_str) {
        Some("cmyk" | "gray") => ["c", "m", "y", "k"].iter().any(|k| color.and_then(|v| v.get(k)).and_then(Value::as_f64).is_some_and(|n| n > 0.0)),
        // Unknown/RGB paint cannot establish absence. Measurement below refuses it.
        _ => true,
    }
}
fn paint_supported(p: &PaintedObject) -> bool {
    p.ink.is_some() || matches!(p.paint.pointer("/color/model").and_then(Value::as_str), Some("cmyk" | "gray"))
}
fn paint_key(p: &PaintedObject) -> Value {
    json!({"ink":p.ink,"tint":p.tint,"paint":p.paint})
}
fn shape(paints: &[&PaintedObject]) -> Result<PathData, String> {
    if paints.iter().any(|p| !paint_supported(p)) {
        return Err("Colour-trap paint model is not supported; positive ink cannot be inferred".into());
    }
    if paints.len() > 200 || paints.iter().filter_map(|p| p.geometry_mm.as_ref()).map(PathData::anchor_count).sum::<usize>() > 4000 {
        return Err("Colour-trap vector measurement budget exceeded".into());
    }
    crate::program::union(
        paints
            .iter()
            .map(|p| p.geometry_mm.clone().ok_or_else(|| "Colour-trap geometry is not decoded".to_string()))
            .collect::<Result<Vec<_>, _>>()?,
    )
}
fn selected<'a>(r: &Request, c: &'a Census, key: &str, page: usize, indigo_only: bool) -> Vec<&'a PaintedObject> {
    c.paints
        .iter()
        .filter(|p| {
            p.page == Some(page)
                && positive(p)
                && r.recipe.file_links.iter().any(|l| {
                    l.source_rule == key
                        && (!indigo_only || l.destination == "Indigo")
                        && r.garment.matches(&l.garment)
                        && crate::routes::paint_matches(l, p)
                })
        })
        .collect()
}
fn target_survives(c: &Census, target: &[&PaintedObject]) -> Result<(), String> {
    let mut comparisons = 0usize;
    for paint in target {
        let shape = paint.geometry_mm.as_ref().ok_or("Trap paint geometry unresolved")?;
        for later in c.paints.iter().filter(|p| p.page == paint.page && p.order > paint.order && (!p.overprint || p.process)) {
            comparisons += 1;
            if comparisons > 2000 {
                return Err("Colour-trap compositing budget exceeded".into());
            }
            let later_shape = later.geometry_mm.as_ref().ok_or("Later paint geometry unresolved")?;
            if !boolean(shape, later_shape, BoolOp::Intersect)?.is_empty() {
                return Err("Later paint may change colour-trap coverage; effective compositing is not measured by this observation".into());
            }
        }
    }
    Ok(())
}
fn exclusions(r: &Request, c: &Census, i: &TrappingIntent) -> Result<(Status, Value), String> {
    if c.pages > 16 || c.budget_exhausted || c.paints.len() > 200 {
        return Err("Exclusion observation budget exceeded".into());
    }
    let regions = i.exclusions.as_ref().ok_or("mixed-trap exclusions not known")?;
    let rules = colour_rules(r);
    if rules.is_empty() {
        return Err("No applicable Indigo colour-trap route declared".into());
    }
    let mut status = Status::Pass;
    let mut rows = vec![];
    for region in regions {
        if region.page >= c.pages || !c.reconciled_pages.contains(&region.page) {
            return Err("No trap region page is absent or not fully decoded".into());
        }
        let [x0, y0, x1, y1] = region.bounds_mm;
        let mask = PathData::from_bezpath(&Rect::new(x0, y0, x1, y1).to_path(0.00001));
        for rule in &rules {
            if r.recipe
                .file_links
                .iter()
                .filter(|l| l.source_rule == rule.rule_key && l.destination == "Indigo" && r.garment.matches(&l.garment))
                .any(|l| c.unresolved_layers.iter().any(|layer| crate::routes::layer_matches(&l.pdf_layer, layer)))
            {
                return Err("An excluded colour-trap component has unresolved paint".into());
            }
            let paints = selected(r, c, &rule.rule_key, region.page, true);
            target_survives(c, &paints)?;
            let area = boolean(&shape(&paints)?, &mask, BoolOp::Intersect)?;
            let error = paints.iter().map(|p| p.geometry_error_mm).fold(0.0, f64::max) + 0.00002;
            let interior = if area.is_empty() {
                false
            } else {
                !vectorcraft_pathops::try_offset_path_for_measurement(&area, -error, 0.00001).map_err(|e| e.to_string())?.is_empty()
            };
            if interior {
                status = Status::Fail;
            } else if !area.is_empty() && status != Status::Fail {
                status = Status::Incomplete;
            }
            rows.push(json!({"page":region.page,"bounds_mm":region.bounds_mm,"rule":rule.rule_key,"bleed_in_known_exclusion":interior}));
        }
    }
    Ok((status, json!(rows)))
}
pub(crate) fn check(r: &Request, c: &Census, report: &mut Report) -> Result<(), String> {
    let parsed = intent(r);
    let (i, why) = match parsed {
        Ok(Some(i)) => (Some(i), ""),
        Ok(None) => (None, "Trapping choice not supplied; Dark/Light does not imply trap intent"),
        Err(_) => (None, "Trapping intent is invalid"),
    };
    let Some(i) = i else {
        return record(r, report, "print/trapping-intent", Status::Incomplete, why, Value::Null);
    };
    let unknown = i.mode == TrapMode::Mixed && (i.exclusions.is_none() || !i.objects.is_empty());
    let mut state = if unknown { Status::Incomplete } else { Status::Pass };
    let mut rows = vec![];
    if i.mode == TrapMode::Mixed && !unknown {
        match exclusions(r, c, &i) {
            Ok((s, observed)) => {
                state = s;
                rows.push(json!({"known_exclusions":observed}));
            }
            Err(why) => {
                state = Status::Incomplete;
                rows.push(json!({"known_exclusions_limitation":why}));
            }
        }
    }
    let rules = colour_rules(r);
    if rules.is_empty() && !unknown {
        state = Status::Incomplete;
    }
    for rule in rules {
        let key = format!("observed/rule/{}/relationship", rule.rule_key);
        if unknown {
            record(
                r,
                report,
                &key,
                Status::Incomplete,
                if i.exclusions.is_none() {
                    "mixed-trap exclusions not known"
                } else {
                    "Native No trap object selections retained; exact object-region resolution is not implemented for final PDF checks"
                },
                Value::Null,
            )?;
            continue;
        }
        let result = measure(r, c, &i, rule);
        match result {
            Ok((status, observed)) => {
                // Intent's Pass only confirms known selections; geometric acceptance stays on the rule.
                if status == Status::Fail {
                    state = Status::Fail;
                }
                rows.push(observed.clone());
                record(
                    r,
                    report,
                    &key,
                    status,
                    "Colour-trap shape, edge colour and excluded-region observations; numerical acceptance uses only the supplied geometry tolerance",
                    observed,
                )?;
            }
            Err(why) => {
                record(r, report, &key, Status::Incomplete, &why, Value::Null)?;
            }
        }
    }
    record(
        r,
        report,
        "print/trapping-intent",
        state,
        if unknown {
            "mixed-trap exclusions not known"
        } else {
            "Explicit trapping choice retained independently of garment; known exclusions apply to colour trapping only"
        },
        json!({"intent":i,"measurements":rows}),
    )
}
fn measure(r: &Request, c: &Census, i: &TrappingIntent, rule: &crate::recipe::Rule) -> Result<(Status, Value), String> {
    let treatment =
        r.recipe.foundation.as_ref().and_then(|f| f.get("treatments")).and_then(|t| t.get(&rule.rule_key)).ok_or("Colour-trap treatment missing")?;
    if i.mode != TrapMode::NoTrap
        && (treatment.get("select") != Some(&json!([[]]))
            || treatment.get("edges") != Some(&json!("all"))
            || treatment.get("neighbours") != Some(&json!([]))
            || !matches!(treatment.get("partition").and_then(Value::as_str), Some("object" | "union")))
    {
        return Err(
            "Filtered or restricted colour-trap edges need explicit source eligibility; final artwork alone cannot prove that selection".into()
        );
    }
    if c.pages > 16 || c.budget_exhausted || c.paints.len() > 200 {
        return Err("Colour-trap observation budget exceeded".into());
    }
    if i.exclusions.as_ref().is_some_and(|v| v.iter().any(|e| e.page >= c.pages)) {
        return Err("No trap region refers to a missing page".into());
    }
    let tolerance = r.acceptance.get("geometry_tolerance_mm").and_then(|f| f.value.as_f64()).filter(|n| n.is_finite() && *n >= 0.0);
    let mut status = if tolerance.is_some() || i.mode == TrapMode::NoTrap { Status::Pass } else { Status::Incomplete };
    let mut rows = vec![];
    for page in 0..c.pages {
        if !c.reconciled_pages.contains(&page) {
            return Err("Colour trapping needs a completely decoded vector page; raster/partial paint remains unmeasured".into());
        }
        let target = selected(r, c, &rule.rule_key, page, true);
        if r.recipe
            .file_links
            .iter()
            .filter(|l| l.source_rule == rule.rule_key && l.destination == "Indigo" && r.garment.matches(&l.garment))
            .any(|l| c.unresolved_layers.iter().any(|layer| crate::routes::layer_matches(&l.pdf_layer, layer)))
        {
            return Err("Colour-trap component has unresolved paint".into());
        }
        target_survives(c, &target)?;
        if i.mode == TrapMode::NoTrap {
            let path = shape(&target)?;
            if !path.is_empty() {
                status = Status::Fail;
            }
            rows.push(json!({"page":page,"unexpected_colour_trap":!path.is_empty()}));
            continue;
        }
        let source_key = rule.sources.first().filter(|_| rule.sources.len() == 1).ok_or("Colour-trap source relationship is not singular")?;
        let source_rule = if source_key.kind == "rule" {
            Some(source_key.key.as_str())
        } else {
            r.recipe
                .rules
                .iter()
                .find(|p| {
                    p.operation == "preserve"
                        && p.sources.len() == 1
                        && p.sources.first().is_some_and(|s| s.kind == source_key.kind && s.key == source_key.key)
                        && r.garment.matches(&p.garment)
                        && r.recipe
                            .foundation
                            .as_ref()
                            .and_then(|f| f.get("treatments"))
                            .and_then(|t| t.get(&p.rule_key))
                            .is_some_and(|t| t.get("select") == Some(&json!([[]])) && t.pointer("/paint/mode") == Some(&json!("source")))
                })
                .map(|p| p.rule_key.as_str())
        }
        .ok_or("A retained, unfiltered source-colour artwork component is needed")?;
        let source = selected(r, c, source_rule, page, true);
        target_survives(c, &source)?;
        if treatment.get("partition") == Some(&json!("object")) && source.len() > 1 {
            return Err("Per-object colour trapping needs separately resolved source edges; a multi-object union is not its expectation".into());
        }
        if source.is_empty() && target.is_empty() {
            continue;
        }
        if source.is_empty() {
            return Err("Colour-trap source artwork is not observable".into());
        }
        let paint = paint_key(source.first().ok_or("Source paint missing")?);
        if source.iter().any(|p| paint_key(p) != paint) {
            return Err(
                "Multiple adjacent source colours need edge ownership; this bounded observation supports one solid source colour per page".into()
            );
        }
        let keys: BTreeSet<_> = source.iter().chain(&target).map(|p| p.layer.as_str()).collect();
        if keys.iter().any(|k| c.unresolved_layers.contains(*k)) {
            return Err("A colour-trap component has unresolved paint".into());
        }
        if rule.join.as_deref() != Some("round") {
            return Err("This colour-trap observation requires a declared round join".into());
        }
        let distance = rule.distance_mm.filter(|n| n.is_finite() && *n > 0.0 && *n <= 10.0).ok_or("Trap distance not declared")?;
        let a = shape(&source)?;
        let settings =
            r.recipe.foundation.as_ref().and_then(|f| f.pointer("/geometry/trapping")).ok_or("Colour-trap construction settings not supplied")?;
        if settings.get("pullback") != Some(&json!(false)) || settings.get("allow_overshoot") != Some(&json!(true)) {
            return Err("Pullback or restricted overshoot needs a generation model not supported by this observation".into());
        }
        // A single convex contour has no internal gaps to close. Do not quietly ignore
        // gap-closing on concave, holed or disconnected source shapes.
        if a.subpaths.len() != 1 {
            return Err("Gap closing on multi-contour artwork is not measured".into());
        }
        let contour = a.subpaths.first().ok_or("Source contour missing")?;
        if !contour.closed || contour.anchors.len() < 3 || contour.anchors.iter().any(|a| a.p != a.h_in || a.p != a.h_out) {
            return Err("Colour-trap gap-closing observation currently needs one convex straight-edged source contour".into());
        }
        let n = contour.anchors.len();
        let mut sign = 0.0f64;
        for j in 0..n {
            let (Some(a), Some(b), Some(c)) = (contour.anchors.get(j), contour.anchors.get((j + 1) % n), contour.anchors.get((j + 2) % n)) else {
                return Err("Invalid contour".into());
            };
            let cross = (b.p - a.p).cross(c.p - b.p);
            if cross != 0.0 {
                if sign != 0.0 && sign.is_sign_positive() != cross.is_sign_positive() {
                    return Err("Gap closing on concave artwork is not measured".into());
                }
                sign = cross;
            }
        }
        let extended = vectorcraft_pathops::try_offset_path_for_measurement(&a, distance, 0.00001).map_err(|e| e.to_string())?;
        let mut expected = boolean(&extended, &a, BoolOp::Difference)?;
        let mut masks = vec![];
        if i.mode == TrapMode::Mixed {
            for region in i.exclusions.iter().flatten().filter(|e| e.page == page) {
                let [x0, y0, x1, y1] = region.bounds_mm;
                masks.push(PathData::from_bezpath(&Rect::new(x0, y0, x1, y1).to_path(0.00001)));
            }
        }
        let excluded = crate::program::union(masks)?;
        expected = boolean(&expected, &excluded, BoolOp::Difference)?;
        for protection in &rule.protection {
            if protection.kind != "rule" {
                return Err("Trap protection needs an explicit observable output".into());
            }
            let protect = shape(&selected(r, c, &protection.key, page, false))?;
            expected = boolean(&expected, &protect, BoolOp::Difference)?;
        }
        let b = shape(&target)?;
        let wrong: Vec<_> = target.iter().copied().filter(|p| paint_key(p) != paint).collect();
        let wrong_shape = shape(&wrong)?;
        let bleed = boolean(&b, &excluded, BoolOp::Intersect)?;
        let error = source.iter().chain(&target).map(|p| p.geometry_error_mm).fold(0.0, f64::max) * 2.0 + 0.00002;
        let interior = |path: &PathData| -> Result<bool, String> {
            if path.is_empty() {
                return Ok(false);
            }
            Ok(!vectorcraft_pathops::try_offset_path_for_measurement(path, -error, 0.00001).map_err(|e| e.to_string())?.is_empty())
        };
        let wrong_colour = interior(&wrong_shape)?;
        let excluded_bleed = interior(&bleed)?;
        if ((!wrong_shape.is_empty() && !wrong_colour) || (!bleed.is_empty() && !excluded_bleed)) && status != Status::Fail {
            status = Status::Incomplete;
        }
        if wrong_colour || excluded_bleed {
            status = Status::Fail;
        }
        let comparison = crate::geometry::compare(&expected, &b, 0.0, 0.0001)?;
        let lower = (comparison.lower_mm - error).max(0.0);
        let upper = comparison.upper_mm + error;
        if let Some(tol) = tolerance {
            if lower > tol
                || comparison.expected_components != comparison.observed_components
                || comparison.expected_holes != comparison.observed_holes
            {
                status = Status::Fail;
            } else if upper > tol && status != Status::Fail {
                status = Status::Incomplete;
            }
        }
        rows.push(json!({"page":page,"distance_mm":distance,"geometry_tolerance_mm":tolerance,"boundary_deviation_mm":[lower,upper],
            "wrong_edge_colour":wrong_colour,"bleed_in_known_exclusion":excluded_bleed,"expected_components":comparison.expected_components,
            "observed_components":comparison.observed_components,"expected_holes":comparison.expected_holes,"observed_holes":comparison.observed_holes,
            "nominal_shape_disagrees":comparison.status==Status::Fail}));
    }
    if rows.is_empty() {
        return Err("No eligible colour-trap geometry observed".into());
    }
    Ok((status, json!({"pages":rows})))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fact(value: Value) -> crate::preparation::Fact {
        crate::preparation::Fact {
            value,
            provenance: crate::preparation::Provenance::Derived {
                artifact_sha256: "a".repeat(64),
                algorithm: "synthetic".into(),
                version: "1".into(),
            },
        }
    }
    fn request(mode: TrapMode, exclusions: Option<Vec<vectorcraft_doc::trapping::TrapRegion>>) -> Request {
        let mut r = crate::attribution::tests::request();
        let mut source = r.recipe.rules[0].clone();
        source.rule_key = "art".into();
        source.operation = "preserve".into();
        source.garment = "any".into();
        source.protection.clear();
        source.sources = vec![crate::recipe::Source { kind: "artwork".into(), key: "selected".into() }];
        let mut trap = source.clone();
        trap.rule_key = "trap".into();
        trap.operation = "spread".into();
        trap.distance_mm = Some(0.2);
        trap.join = Some("round".into());
        r.recipe.rules = vec![source, trap];
        let mut art = r.recipe.file_links[0].clone();
        art.key = "art".into();
        art.source_rule = "art".into();
        art.garment = "any".into();
        art.destination = "Indigo".into();
        art.name_mode = "source".into();
        art.separation_name = None;
        art.pdf_layer = "Artwork".into();
        let mut trap = art.clone();
        trap.key = "trap".into();
        trap.source_rule = "trap".into();
        trap.pdf_layer = "Auto Trap-1".into();
        r.recipe.file_links = vec![art, trap];
        r.recipe.required_screens = Some(vec![]);
        let f = r.recipe.foundation.as_mut().unwrap();
        f.as_object_mut().unwrap().remove("verification");
        f["treatments"] = json!({"art":{"select":[[]],"edges":"all","neighbours":[],"protection":"none","paint":{"mode":"source"},"partition":"object","result":"full_shape"},"trap":{"select":[[]],"edges":"all","neighbours":[],"protection":"none","paint":{"mode":"source"},"partition":"object","result":"outer_band"}});
        f["outputs"] = json!({"art":{"select":"all","emit":"when_nonempty"},"trap":{"select":"all","emit":"when_nonempty"}});
        f["geometry"]["trapping"] = json!({"close_gaps_mm":0.01,"allow_overshoot":true,"pullback":false});
        r.acceptance.clear();
        r.acceptance.insert("trapping_intent".into(), fact(json!(TrappingIntent { mode, exclusions, objects: vec![] })));
        // Test-only acceptance, never a production default.
        r.acceptance.insert("geometry_tolerance_mm".into(), fact(json!(0.002)));
        r
    }
    fn pdf(distance: f64, wrong: bool, clip: &str) -> Vec<u8> {
        // Original hand-authored PDF paths. Quarter-circle corners use the public cubic circle approximation.
        let d = distance;
        let lo = 10.0 - d;
        let hi = 20.0 + d;
        let k = d * 0.5522847498307936;
        let paint = if wrong { "1 0 0 0" } else { "0 1 0 0" };
        let stream = format!(
            "2.834645669291339 0 0 2.834645669291339 0 0 cm /OC /A BDC 0 1 0 0 k 10 10 10 10 re f EMC /OC /T BDC q {clip} {paint} k 10 {lo} m 20 {lo} l {} {lo} {hi} {} {hi} 10 c {hi} 20 l {hi} {} {} {hi} 20 {hi} c 10 {hi} l {} {hi} {lo} {} {lo} 20 c {lo} 10 l {lo} {} {} {lo} 10 {lo} c h 10 10 10 10 re f* Q EMC",
            20.0 + k,
            10.0 - k,
            20.0 + k,
            20.0 + k,
            10.0 - k,
            20.0 + k,
            10.0 - k,
            10.0 - k
        );
        crate::pdf::tests::objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R] /D << /ON [5 0 R 6 0 R] >> >> >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 283.4645669291339 283.4645669291339] /Resources << /Properties << /A 5 0 R /T 6 0 R >> >> /Contents 4 0 R >>".into(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream",stream.len()),
            "<< /Type /OCG /Name (Artwork) >>".into(),"<< /Type /OCG /Name (Auto Trap-1) >>".into()])
    }
    fn finding(r: &Request, bytes: &[u8]) -> crate::Finding {
        let report = crate::print_quality::check(r, bytes).unwrap();
        report.findings.into_iter().find(|f| f.obligation.key == "observed/rule/trap/relationship").unwrap()
    }
    #[test]
    fn correct_trap_passes_wrong_distance_colour_and_gap_fail() {
        let r = request(TrapMode::Trap, Some(vec![]));
        let good = finding(&r, &pdf(0.2, false, ""));
        assert_eq!(good.status, Status::Pass, "{good:?}");
        for bytes in [pdf(0.3, false, ""), pdf(0.2, true, ""), pdf(0.2, false, "0 0 100 100 re 19.9 14 1 2 re W* n")] {
            let f = finding(&r, &bytes);
            assert_eq!(f.status, Status::Fail, "{f:?}");
        }
    }
    #[test]
    fn known_mixed_region_control_and_bleed_negative() {
        let r = request(TrapMode::Mixed, Some(vec![vectorcraft_doc::trapping::TrapRegion { page: 0, bounds_mm: [20.0, 0.0, 50.0, 100.0] }]));
        let good = finding(&r, &pdf(0.2, false, "0 0 20 100 re W n"));
        assert_eq!(good.status, Status::Pass, "{good:?}");
        let bad = finding(&r, &pdf(0.2, false, ""));
        assert_eq!(bad.status, Status::Fail, "{bad:?}");
        assert_eq!(bad.observed["pages"][0]["bleed_in_known_exclusion"], json!(true));
    }
    #[test]
    fn occluded_sources_overwritten_targets_and_object_unions_remain_incomplete() {
        let r = request(TrapMode::Trap, Some(vec![]));
        let imported = vectorcraft_pdf::import_with_report(&pdf(0.2, false, ""), &Default::default()).unwrap();
        let mut c = crate::pdf::census(&imported.document);
        // Use decoded native paints; mark the already reconciled synthetic page for these compositing mutations.
        c.reconciled_pages.insert(0);
        let trap = r.recipe.rules.iter().find(|r| r.rule_key == "trap").unwrap();
        let intent = intent(&r).unwrap().unwrap();
        let source = c.paints.iter().find(|p| p.layer == "Artwork").unwrap().clone();
        let mut white = source.clone();
        white.id = 999;
        white.order = 100;
        white.paint = json!({"color":{"model":"cmyk","c":0,"m":0,"y":0,"k":0}});
        white.geometry_mm = Some(PathData::from_bezpath(&Rect::new(12.0, 82.0, 18.0, 88.0).to_path(0.00001)));
        c.paints.push(white);
        assert!(measure(&r, &c, &intent, trap).unwrap_err().contains("Later paint"));
        c.paints.pop();
        let mut over = c.paints.iter().find(|p| p.layer == "Auto Trap-1").unwrap().clone();
        over.id = 998;
        over.order = 101;
        c.paints.push(over);
        assert!(measure(&r, &c, &intent, trap).unwrap_err().contains("Later paint"));
        c.paints.pop();
        let mut source2 = source.clone();
        source2.id = 997;
        source2.order = source.order;
        c.paints.push(source2);
        assert!(measure(&r, &c, &intent, trap).unwrap_err().contains("multi-object"));
    }
    #[test]
    fn unsupported_generator_and_selection_overrides_cannot_pass() {
        for (field, value) in [("pullback", json!(true)), ("allow_overshoot", json!(false))] {
            let mut r = request(TrapMode::Trap, Some(vec![]));
            r.recipe.foundation.as_mut().unwrap()["geometry"]["trapping"][field] = value;
            assert_eq!(finding(&r, &pdf(0.2, false, "")).status, Status::Incomplete);
        }
        let mut r = request(TrapMode::Trap, Some(vec![]));
        r.recipe.foundation.as_mut().unwrap()["treatments"]["trap"]["edges"] = json!("shared_only");
        assert_eq!(finding(&r, &pdf(0.2, false, "")).status, Status::Incomplete);
        r.recipe.file_links.iter_mut().find(|l| l.key == "trap").unwrap().destination = "Screen".into();
        assert!(colour_rules(&r).is_empty());
    }
    #[test]
    fn excluded_regions_measure_independently_of_source_selection_and_use_top_left_page_mm() {
        let mut r = request(TrapMode::Mixed, Some(vec![vectorcraft_doc::trapping::TrapRegion { page: 0, bounds_mm: [0.0, 0.0, 100.0, 85.0] }]));
        r.recipe.foundation.as_mut().unwrap()["treatments"]["trap"]["select"] = json!([[{"field":"role","operator":"equals","value":"ordinary"}]]);
        let status = |bytes: Vec<u8>| {
            crate::print_quality::check(&r, &bytes)
                .unwrap()
                .findings
                .into_iter()
                .find(|f| f.obligation.key == "print/trapping-intent")
                .unwrap()
                .status
        };
        assert_eq!(status(pdf(0.2, false, "0 0 100 15 re W n")), Status::Pass);
        assert_eq!(status(pdf(0.2, false, "")), Status::Fail);
    }
    #[test]
    fn unknown_mixed_and_missing_tolerance_do_not_invent_acceptance() {
        let r = request(TrapMode::Mixed, None);
        let f = finding(&r, &pdf(0.2, false, ""));
        assert_eq!(f.status, Status::Incomplete);
        assert_eq!(f.reason, "mixed-trap exclusions not known");
        let report = crate::print_quality::check(&r, &pdf(0.2, false, "")).unwrap();
        assert!(report.findings.iter().any(|f| f.obligation.key == "print/inks" && f.status == Status::Pass));
        let mut r = request(TrapMode::Trap, Some(vec![]));
        r.acceptance.remove("geometry_tolerance_mm");
        assert_eq!(finding(&r, &pdf(0.2, false, "")).status, Status::Incomplete);
        let r = request(TrapMode::NoTrap, Some(vec![]));
        assert_eq!(finding(&r, &pdf(0.2, false, "")).status, Status::Fail);
    }
}
