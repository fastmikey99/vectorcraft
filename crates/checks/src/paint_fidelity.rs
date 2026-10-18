//! Compare colour-bearing coverage, so equal outlines with swapped paint cannot pass.
//! This compares decoded solid components; profile/colour-space definition qualification
//! stays in the global colour obligation. No display RGB conversion is performed.
use crate::{
    Evidence, Location, Report, Request, Status,
    pdf::Census,
    program::{self, Program},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use vectorcraft_geom::{PathData, Shape};
#[derive(Clone, Debug, Serialize)]
struct Colour {
    ink: Option<String>,
    model: String,
    components: Vec<f64>,
    tint: f64,
}
impl Colour {
    fn read(paint: &Value, ink: Option<&str>) -> Result<Self, String> {
        if paint.get("mode").and_then(Value::as_str) == Some("output_ink") {
            return Err("Technical-ink tint criteria need an explicit confirmed requirement".into());
        }
        if paint.get("mode").and_then(Value::as_str) == Some("cmyk") {
            let values = paint.get("value").and_then(Value::as_array).ok_or("Assigned CMYK values missing")?;
            if values.len() != 4 {
                return Err("Assigned CMYK needs four components".into());
            }
            let components = values
                .iter()
                .map(|v| {
                    v.as_f64().filter(|n| n.is_finite() && (0.0..=100.0).contains(n)).map(|n| n / 100.0).ok_or("Invalid CMYK assignment".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Self { ink: None, model: "cmyk".into(), components, tint: 1.0 });
        }
        if paint.get("type").and_then(Value::as_str) != Some("solid") {
            return Err("Source paint needs qualified non-solid evaluation".into());
        }
        let tint = paint.get("tint").and_then(Value::as_f64).unwrap_or(1.0);
        if !tint.is_finite() || !(0.0..=1.0).contains(&tint) {
            return Err("Invalid source tint".into());
        }
        let color = paint.get("color").ok_or("Source colour missing")?;
        let model = color.get("model").and_then(Value::as_str).ok_or("Source colour model missing")?;
        let keys: &[&str] = match model {
            "cmyk" => &["c", "m", "y", "k"],
            "rgb" => &["r", "g", "b"],
            "gray" => &["k"],
            "lab" => &["l", "a", "b"],
            _ => return Err("Unqualified source colour model".into()),
        };
        let components = keys
            .iter()
            .map(|k| color.get(*k).and_then(Value::as_f64).filter(|n| n.is_finite()).ok_or("Invalid decoded colour component".to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { ink: ink.map(String::from), model: model.into(), components, tint })
    }
    fn agrees(&self, other: &Self) -> bool {
        // A conservative f32 decoding envelope, not a manufacturing colour allowance.
        let equal = |a: f64, b: f64| (a - b).abs() <= 4.0 * f64::from(f32::EPSILON) * a.abs().max(b.abs()).max(1.0);
        self.ink == other.ink
            && self.model == other.model
            && self.components.len() == other.components.len()
            && equal(self.tint, other.tint)
            && self.components.iter().zip(&other.components).all(|(a, b)| equal(*a, *b))
    }
}
struct Group {
    colour: Colour,
    page: usize,
    expected: Vec<PathData>,
    observed: Vec<PathData>,
    error: f64,
    observed_error: f64,
    objects: Vec<u64>,
}
#[derive(Serialize)]
struct LayerResult {
    layer: String,
    status: Status,
    observed: Value,
    location: Option<Location>,
}
fn combine_status(a: Status, b: Status) -> Status {
    if a == Status::Fail || b == Status::Fail {
        Status::Fail
    } else if a == Status::Incomplete || b == Status::Incomplete {
        Status::Incomplete
    } else {
        Status::Pass
    }
}
/// Exact opaque knockout in page space. Later paint removes earlier visible
/// coverage, including holes; no image sampling or area suppression is involved.
fn visible_knockout(paths: &[(usize, usize, PathData)]) -> Result<(Vec<PathData>, f64), String> {
    use vectorcraft_geom::FillRule;
    use vectorcraft_pathops::{BoolOp, try_boolean_for_measurement};
    if paths.len() > 2500 {
        return Err("Opaque paint-stack measurement budget exceeded".into());
    }
    let mut indices: Vec<_> = (0..paths.len()).collect();
    indices.sort_by_key(|i| paths.get(*i).map(|p| (p.0, p.1)));
    let mut result = vec![PathData::default(); paths.len()];
    let mut later: BTreeMap<usize, PathData> = BTreeMap::new();
    let mut operations = 0;
    for i in indices.into_iter().rev() {
        let (page, _, path) = paths.get(i).ok_or("Paint-stack index missing")?;
        let mask = later.entry(*page).or_default();
        let visible = if mask.is_empty() {
            path.clone()
        } else {
            operations += 1;
            try_boolean_for_measurement(path, FillRule::NonZero, mask, FillRule::NonZero, BoolOp::Difference).map_err(|e| e.to_string())?
        };
        *result.get_mut(i).ok_or("Paint-stack output index missing")? = visible;
        if mask.is_empty() {
            *mask = path.clone();
        } else {
            operations += 1;
            *mask = try_boolean_for_measurement(mask, FillRule::NonZero, path, FillRule::NonZero, BoolOp::Union).map_err(|e| e.to_string())?;
        }
    }
    // Construction precision is separate from manufacturing acceptance. Retain
    // a conservative cumulative bound for every boolean operation in the stack.
    Ok((result, f64::from(operations) * 0.00001))
}
pub fn check(request: &Request, program: &Program, census: &Census, report: &mut Report) -> Result<(), String> {
    if !census.paint_order_verified || !census.unresolved.is_empty() || census.budget_exhausted {
        return Ok(());
    }
    let links: Vec<_> = request.recipe.file_links.iter().filter(|l| request.garment.matches(&l.garment)).collect();
    let layers: BTreeSet<_> = links.iter().map(|l| l.pdf_layer.as_str()).collect();
    let tolerance = request
        .acceptance
        .get("geometry_tolerance_mm")
        .filter(|f| f.evidenced())
        .and_then(|f| f.value.as_f64())
        .filter(|t| t.is_finite() && *t >= 0.0)
        .filter(|_| crate::digest(&request.acceptance).is_ok_and(|h| h == request.identity.measurement_sha256));
    let evidence = vec![
        Evidence {
            artifact_sha256: request.identity.pdf_sha256.clone(),
            producer: "checks/paint-region-1".into(),
            detail: "Decoded source model/components/tints and per-colour page-space coverage; profile definitions remain a separate requirement"
                .into(),
        },
        Evidence {
            artifact_sha256: request.identity.baseline_sha256.clone(),
            producer: "checks/approved-source-1".into(),
            detail: "Expected paints come from immutable source regions and explicit recipe assignments".into(),
        },
    ];
    let mut summaries = vec![];
    let mut any_fail = false;
    let mut rule_status: BTreeMap<String, Status> = BTreeMap::new();
    for layer in layers {
        let declared: Vec<_> = links.iter().filter(|l| l.pdf_layer == layer).collect();
        let result = (|| -> Result<(Status, Value, Option<Location>), String> {
            if declared.iter().any(|l| !program.outputs.contains_key(&l.key)) {
                return Err("Source construction or paint assignment remains unresolved".into());
            }
            let expected: Vec<_> =
                declared.iter().flat_map(|l| program.outputs.get(&l.key).into_iter().flatten()).filter(|p| !p.path.is_empty()).collect();
            let observed: Vec<_> =
                census.paints.iter().filter(|p| p.layer == layer && p.geometry_mm.as_ref().is_some_and(|g| !g.is_empty())).collect();
            if observed.len() > 2500 {
                return Err("Output paint-stack measurement budget exceeded".into());
            }
            if expected.is_empty() && observed.is_empty() {
                return Err("Empty contributions retain their independent applicability evidence".into());
            }
            let orders: Option<Vec<_>> = expected
                .iter()
                .map(|p| {
                    let values: Option<BTreeSet<_>> = p.regions.iter().map(|id| request.geometry.as_ref()?.regions.get(id)?.source_order).collect();
                    let values = values?;
                    if values.len() == 1 { values.first().copied() } else { None }
                })
                .collect();
            let composited = declared.len() == 1
                && declared.first().is_some_and(|l| !l.overprint)
                && observed.iter().all(|p| !p.overprint)
                && census.paint_order_verified
                && orders.as_ref().is_some_and(|o| o.iter().collect::<BTreeSet<_>>().len() == o.len());
            let (expected_paths, observed_paths, stack_error) = if composited {
                let expected_stack: Vec<_> = expected
                    .iter()
                    .zip(orders.as_ref().ok_or("Source paint order missing")?)
                    .map(|(p, order)| (p.page, *order, p.path.clone()))
                    .collect();
                let observed_stack = observed
                    .iter()
                    .map(|p| Ok((p.page.ok_or("Output page unresolved")?, p.order, p.geometry_mm.clone().ok_or("Output geometry unresolved")?)))
                    .collect::<Result<Vec<_>, String>>()?;
                let (a, a_error) = visible_knockout(&expected_stack)?;
                let (b, b_error) = visible_knockout(&observed_stack)?;
                let source_error = expected.iter().map(|p| p.construction_error_mm).fold(0.0, f64::max);
                let output_error = observed.iter().map(|p| p.geometry_error_mm).fold(0.0, f64::max);
                (a, b, a_error + b_error + source_error + output_error)
            } else {
                (expected.iter().map(|p| p.path.clone()).collect(), observed.iter().filter_map(|p| p.geometry_mm.clone()).collect(), 0.0)
            };
            let mut groups: Vec<Group> = vec![];
            for (p, path) in expected.iter().zip(expected_paths) {
                if path.is_empty() {
                    continue;
                }
                let colour = Colour::read(&p.paint, p.ink.as_deref())?;
                let matches: Vec<_> =
                    groups.iter().enumerate().filter(|(_, g)| g.page == p.page && g.colour.agrees(&colour)).map(|(i, _)| i).collect();
                if matches.len() > 1 {
                    return Err("Source colours overlap the decoder's numerical envelope".into());
                }
                if let Some(g) = matches.first().and_then(|i| groups.get_mut(*i)) {
                    g.expected.push(path);
                    g.error = g.error.max(p.construction_error_mm + stack_error);
                } else {
                    groups.push(Group {
                        colour,
                        page: p.page,
                        expected: vec![path],
                        observed: vec![],
                        error: p.construction_error_mm + stack_error,
                        observed_error: 0.0,
                        objects: vec![],
                    });
                }
                if groups.len() > 1024 {
                    return Err("Per-colour measurement palette budget exceeded".into());
                }
            }
            // Occluded source paints may legally disappear during flattening. Until
            // source-order compositing is established, do not demand hidden colour areas.
            let unions = groups.iter().map(|g| program::union(g.expected.clone())).collect::<Result<Vec<_>, _>>()?;
            let mut intersections = 0;
            for (i, a) in unions.iter().enumerate() {
                for (j, b) in unions.iter().enumerate().skip(i + 1) {
                    if groups.get(i).map(|g| g.page) != groups.get(j).map(|g| g.page) {
                        continue;
                    }
                    let a_box = a.to_bezpath().bounding_box();
                    let b_box = b.to_bezpath().bounding_box();
                    if a_box.intersect(b_box).area() <= 0.0 {
                        continue;
                    }
                    intersections += 1;
                    if intersections > 5000 {
                        return Err("Source-colour overlap measurement budget exceeded".into());
                    }
                    let overlap = vectorcraft_pathops::try_boolean_for_measurement(
                        a,
                        vectorcraft_geom::FillRule::NonZero,
                        b,
                        vectorcraft_geom::FillRule::NonZero,
                        vectorcraft_pathops::BoolOp::Intersect,
                    )
                    .map_err(|e| e.to_string())?;
                    if !overlap.is_empty() {
                        return Err(
                            "Overlapping source colours require original-order compositing before flattened paint fidelity can be established".into(),
                        );
                    }
                }
            }
            if !composited {
                // An unverified output stack can hide an otherwise wrong colour.
                // Do not turn hidden paint into a confirmed fidelity failure.
                for (i, a) in observed.iter().enumerate() {
                    for b in observed.iter().skip(i + 1) {
                        if a.page != b.page || Colour::read(&a.paint, a.ink.as_deref())?.agrees(&Colour::read(&b.paint, b.ink.as_deref())?) {
                            continue;
                        }
                        let a = a.geometry_mm.as_ref().ok_or("Output geometry missing")?;
                        let b = b.geometry_mm.as_ref().ok_or("Output geometry missing")?;
                        if a.to_bezpath().bounding_box().intersect(b.to_bezpath().bounding_box()).area() <= 0.0 {
                            continue;
                        }
                        intersections += 1;
                        if intersections > 5000 {
                            return Err("Output-colour overlap measurement budget exceeded".into());
                        }
                        let overlap = vectorcraft_pathops::try_boolean_for_measurement(
                            a,
                            vectorcraft_geom::FillRule::NonZero,
                            b,
                            vectorcraft_geom::FillRule::NonZero,
                            vectorcraft_pathops::BoolOp::Intersect,
                        )
                        .map_err(|e| e.to_string())?;
                        if !overlap.is_empty() {
                            return Err("Overlapping output colours need verified opaque paint-order compositing".into());
                        }
                    }
                }
            }
            let mut wrong = vec![];
            let mut location = None;
            let mut wrong_count = 0usize;
            for (p, path) in observed.iter().zip(observed_paths) {
                if path.is_empty() {
                    continue;
                }
                let colour = Colour::read(&p.paint, p.ink.as_deref())?;
                let page = p.page.ok_or("Output page unresolved")?;
                let matches: Vec<_> = groups.iter().enumerate().filter(|(_, g)| g.page == page && g.colour.agrees(&colour)).map(|(i, _)| i).collect();
                if matches.len() > 1 {
                    return Err("Output colour is numerically ambiguous between expected colours".into());
                }
                if let Some(g) = matches.first().and_then(|i| groups.get_mut(*i)) {
                    g.observed.push(path);
                    g.observed_error = g.observed_error.max(p.geometry_error_mm);
                    if g.objects.len() < 64 {
                        g.objects.push(p.id);
                    }
                } else {
                    wrong_count += 1;
                    if wrong.len() < 64 {
                        wrong.push(json!({"page":page,"object":p.id,"paint":colour}));
                    }
                    if location.is_none() {
                        location = Some(Location {
                            page,
                            bounds_mm: p.geometry_mm.as_ref().map(|p| {
                                let b = p.to_bezpath().bounding_box();
                                [b.x0, b.y0, b.x1, b.y1]
                            }),
                            object_ids: vec![p.id],
                        });
                    }
                }
            }
            let mut status = if wrong_count > 0 { Status::Fail } else { Status::Pass };
            let mut measurements = vec![];
            for g in groups {
                let Some(tolerance) = tolerance else {
                    status = combine_status(status, Status::Incomplete);
                    continue;
                };
                let expected = program::union(g.expected)?;
                let observed = program::union(g.observed)?;
                match crate::geometry::compare(&expected, &observed, tolerance, (tolerance / 16.0).clamp(0.000001, 0.001)) {
                    Ok(mut m) => {
                        m.lower_mm = (m.lower_mm - g.error - g.observed_error).max(0.0);
                        m.upper_mm += g.error + g.observed_error;
                        m.uncertainty_mm = m.upper_mm - m.lower_mm;
                        m.status = if m.expected_components != m.observed_components || m.expected_holes != m.observed_holes || m.lower_mm > tolerance
                        {
                            Status::Fail
                        } else if m.upper_mm <= tolerance {
                            Status::Pass
                        } else {
                            Status::Incomplete
                        };
                        status = combine_status(status, m.status);
                        if m.status == Status::Fail && location.is_none() {
                            location = Some(Location {
                                page: g.page,
                                bounds_mm: m.witness_mm.map(|[x, y]| [x - 1.0, y - 1.0, x + 1.0, y + 1.0]),
                                object_ids: g.objects,
                            });
                        }
                        measurements.push(json!({"page":g.page,"paint":g.colour,"geometry":m}));
                    }
                    Err(reason) => {
                        status = combine_status(status, Status::Incomplete);
                        measurements.push(json!({"page":g.page,"reason":reason}));
                    }
                }
            }
            Ok((
                status,
                json!({"wrong_paint_count":wrong_count,"wrong_paint_sample":wrong,"per_colour_coverage":measurements,"opaque_knockout_composited":composited,"stack_error_mm":stack_error,"decoder_relative_envelope":4.0*f64::from(f32::EPSILON)}),
                location,
            ))
        })();
        let (status, observed, location) = match result {
            Ok(v) => v,
            Err(reason) => (Status::Incomplete, json!({"reason":reason}), None),
        };
        any_fail |= status == Status::Fail;
        let sources: BTreeSet<_> = declared.iter().map(|l| &l.source_rule).collect();
        let attributed = if status == Status::Fail && sources.len() > 1 { Status::Incomplete } else { status };
        for l in declared {
            rule_status.entry(l.source_rule.clone()).and_modify(|s| *s = combine_status(*s, attributed)).or_insert(attributed);
        }
        summaries.push(LayerResult { layer: layer.into(), status, observed, location });
    }
    for (key, status) in rule_status {
        if report.findings.iter().any(|f| f.obligation.key == format!("rule/{key}/paint") && f.status == Status::NotApplicable) {
            continue;
        }
        report.record(&format!("rule/{key}/paint"),status,"Decoded colour-bearing regions compared with authorised source/assignment paint",json!({"layers":summaries.iter().filter(|s|request.recipe.file_links.iter().any(|l|l.source_rule==key && s.layer==l.pdf_layer)).collect::<Vec<_>>()}),evidence.clone())?;
        if let Some(f) = report.findings.iter_mut().find(|f| f.obligation.key == format!("rule/{key}/paint")) {
            f.location = summaries
                .iter()
                .filter(|s| request.recipe.file_links.iter().any(|l| l.source_rule == key && s.layer == l.pdf_layer))
                .find_map(|s| s.location.clone());
        }
    }
    report.record(
        "global/colour",
        if any_fail { Status::Fail } else { Status::Incomplete },
        "Solid paint and region findings retained; exact profile and colour-space definitions still require reconciliation",
        json!({"layers":summaries}),
        evidence,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        preparation::{Fact, Provenance},
        program::Piece,
    };
    use vectorcraft_geom::{BezPath, PathData};
    fn fixture(stream: &str) -> Vec<u8> {
        let content = format!("/OC /Layer BDC {stream} EMC");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] /D << /ON [5 0 R] >> >> >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /Layer 5 0 R >> >> /Contents 4 0 R >>".into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /OCG /Name (Layer) >>".into(),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![0];
        for (i, o) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
        for o in offsets.iter().skip(1) {
            bytes.extend(format!("{o:010} 00000 n \n").as_bytes());
        }
        bytes.extend(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len()).as_bytes());
        bytes
    }
    fn expected() -> Program {
        let k = 25.4 / 72.0;
        let mut pieces = vec![];
        for (x, colour) in [(0.0, [100, 0, 0, 0]), (10.0, [0, 100, 0, 0])] {
            let mut p = BezPath::new();
            p.move_to((x * k, 90.0 * k));
            p.line_to(((x + 10.0) * k, 90.0 * k));
            p.line_to(((x + 10.0) * k, 100.0 * k));
            p.line_to((x * k, 100.0 * k));
            p.close_path();
            pieces.push(Piece {
                page: 0,
                path: PathData::from_bezpath(&p),
                regions: BTreeSet::new(),
                ink: None,
                paint: json!({"mode":"cmyk","value":colour}),
                construction_error_mm: 0.00001,
            });
        }
        Program { outputs: BTreeMap::from([("output".into(), pieces)]), ..Default::default() }
    }
    #[test]
    fn same_outline_with_swapped_or_substituted_colours_fails_and_legal_splitting_passes() {
        for (content, status) in [
            ("1 0 0 0 k 0 0 10 10 re f 0 1 0 0 k 10 0 10 10 re f", Status::Pass),
            ("1 0 0 0 k 0 0 5 10 re f 5 0 5 10 re f 0 1 0 0 k 10 0 10 10 re f", Status::Pass),
            ("0 1 0 0 k 0 0 10 10 re f 1 0 0 0 k 10 0 10 10 re f", Status::Fail),
            ("0.5 0 0 0 k 0 0 10 10 re f 0 1 0 0 k 10 0 10 10 re f", Status::Fail),
            ("1 0 0 rg 0 0 10 10 re f 0 1 0 0 k 10 0 10 10 re f", Status::Fail),
            ("1 0 0 0 k 0 0 10 10 re f", Status::Fail),
        ] {
            let bytes = fixture(content);
            let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
            crate::ordering::try_reconcile(&bytes, &mut c).unwrap();
            assert!(c.paint_order_verified);
            assert!(c.unresolved.is_empty(), "{:?}", c.unresolved);
            let mut r = crate::attribution::tests::request();
            r.acceptance.insert(
                "geometry_tolerance_mm".into(),
                Fact {
                    value: json!(0.02),
                    provenance: Provenance::Operator {
                        actor: "Synthetic author".into(),
                        at: "2026-10-18".into(),
                        decision_id: "Synthetic criterion only".into(),
                        evidence_sha256: "a".repeat(64),
                    },
                },
            );
            r.identity.measurement_sha256 = crate::digest(&r.acceptance).unwrap();
            let mut report = crate::begin(&r).unwrap();
            check(&r, &expected(), &c, &mut report).unwrap();
            let f = report.findings.iter().find(|f| f.obligation.key == "rule/arbitrary/paint").unwrap();
            assert_eq!(f.status, status, "{content}: {}", serde_json::to_string_pretty(f).unwrap());
            if status == Status::Fail {
                assert!(f.location.is_some());
            }
            assert_ne!(report.status, Status::Pass);
        }
    }
    fn overlapping() -> (Request, Program) {
        let mut r = crate::attribution::tests::request();
        r.recipe.file_links[0].overprint = false;
        r.acceptance.insert(
            "geometry_tolerance_mm".into(),
            Fact {
                value: json!(0.02),
                provenance: Provenance::Operator {
                    actor: "Synthetic author".into(),
                    at: "2026-10-18".into(),
                    decision_id: "Synthetic criterion only".into(),
                    evidence_sha256: "a".repeat(64),
                },
            },
        );
        r.identity.measurement_sha256 = crate::digest(&r.acceptance).unwrap();
        let mut p = expected();
        let pieces = p.outputs.get_mut("output").unwrap();
        pieces[0].path = program::union(pieces.iter().map(|p| p.path.clone())).unwrap();
        let mut regions = BTreeMap::new();
        for (order, piece) in pieces.iter_mut().enumerate() {
            let id = format!("original-{order}");
            piece.regions.insert(id.clone());
            regions.insert(
                id,
                program::RegionGeometry {
                    prepared_mask: None,
                    page: piece.page,
                    path_mm: piece.path.clone(),
                    fill_rule: vectorcraft_geom::FillRule::NonZero,
                    paint: piece.paint.clone(),
                    geometry_error_mm: 0.0,
                    source_order: Some(order),
                },
            );
        }
        r.geometry = Some(program::Inputs { regions });
        (r, p)
    }
    fn paint_status(r: &Request, p: &Program, c: &Census) -> Status {
        let mut report = crate::begin(r).unwrap();
        check(r, p, c, &mut report).unwrap();
        report.findings.iter().find(|f| f.obligation.key == "rule/arbitrary/paint").unwrap().status
    }
    #[test]
    fn opaque_source_order_accepts_flattening_and_rejects_reversed_visible_paint() {
        let (r, p) = overlapping();
        for (stream, expected) in [
            ("1 0 0 0 k 0 0 20 10 re f 0 1 0 0 k 10 0 10 10 re f", Status::Pass),
            ("1 0 0 0 k 0 0 10 10 re f 0 1 0 0 k 10 0 10 10 re f", Status::Pass),
            ("0 1 0 0 k 10 0 10 10 re f 1 0 0 0 k 0 0 20 10 re f", Status::Fail),
            ("0 0 1 0 k 0 0 20 10 re f 1 0 0 0 k 0 0 10 10 re f 0 1 0 0 k 10 0 10 10 re f", Status::Pass),
        ] {
            let bytes = fixture(stream);
            let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
            crate::ordering::try_reconcile(&bytes, &mut c).unwrap();
            assert!(c.paint_order_verified);
            assert_eq!(paint_status(&r, &p, &c), expected, "{stream}");
        }
    }
    #[test]
    fn opaque_overlap_needs_bound_source_order_and_verified_non_overprinting_output() {
        let (r, p) = overlapping();
        let bytes = fixture("1 0 0 0 k 0 0 20 10 re f 0 1 0 0 k 10 0 10 10 re f");
        let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        assert_eq!(paint_status(&r, &p, &c), Status::Incomplete);
        crate::ordering::try_reconcile(&bytes, &mut c).unwrap();
        assert!(c.paint_order_verified);
        assert_eq!(paint_status(&r, &p, &c), Status::Pass);
        let mut missing = r.clone();
        missing.geometry.as_mut().unwrap().regions.get_mut("original-0").unwrap().source_order = None;
        assert_eq!(paint_status(&missing, &p, &c), Status::Incomplete);
        c.paints[1].overprint = true;
        assert_eq!(paint_status(&r, &p, &c), Status::Incomplete);
    }
    #[test]
    fn unknown_source_order_does_not_blame_hidden_paint() {
        let (mut r, _) = overlapping();
        r.geometry = None;
        let bytes = fixture("0 0 1 0 k 0 0 20 10 re f 1 0 0 0 k 0 0 10 10 re f 0 1 0 0 k 10 0 10 10 re f");
        let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        crate::ordering::try_reconcile(&bytes, &mut c).unwrap();
        assert!(c.paint_order_verified);
        assert_eq!(paint_status(&r, &expected(), &c), Status::Incomplete);
    }
    #[test]
    fn knockout_hole_exposes_the_underlying_colour() {
        use vectorcraft_geom::{FillRule, Rect};
        use vectorcraft_pathops::{BoolOp, try_boolean_for_measurement};
        let outer = PathData::from_bezpath(&Rect::new(0.0, 0.0, 10.0, 10.0).to_path(0.001));
        let hole = PathData::from_bezpath(&Rect::new(3.0, 3.0, 7.0, 7.0).to_path(0.001));
        let ring = try_boolean_for_measurement(&outer, FillRule::NonZero, &hole, FillRule::NonZero, BoolOp::Difference).unwrap();
        let (visible, _) = visible_knockout(&[(0, 0, outer), (0, 1, ring)]).unwrap();
        assert!(try_boolean_for_measurement(&visible[0], FillRule::NonZero, &hole, FillRule::NonZero, BoolOp::Xor).unwrap().is_empty());
    }
    #[test]
    fn knockout_never_masks_a_different_page() {
        let p = expected();
        let left = p.outputs["output"][0].path.clone();
        let right = p.outputs["output"][1].path.clone();
        let full = program::union([left.clone(), right.clone()]).unwrap();
        let (visible, error) = visible_knockout(&[(0, 0, full.clone()), (1, 1, full.clone()), (0, 2, right)]).unwrap();
        assert!(error > 0.0);
        assert_eq!(visible[1], full);
        let difference = vectorcraft_pathops::try_boolean_for_measurement(
            &visible[0],
            vectorcraft_geom::FillRule::NonZero,
            &left,
            vectorcraft_geom::FillRule::NonZero,
            vectorcraft_pathops::BoolOp::Xor,
        )
        .unwrap();
        assert!(difference.is_empty());
    }
    #[test]
    fn missing_geometry_criteria_or_unspecified_technical_tint_cannot_pass_paint() {
        let bytes = fixture("1 0 0 0 k 0 0 10 10 re f 0 1 0 0 k 10 0 10 10 re f");
        let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        crate::ordering::try_reconcile(&bytes, &mut c).unwrap();
        assert!(c.paint_order_verified);
        let mut r = crate::attribution::tests::request();
        let mut p = expected();
        for unknown_tint in [false, true] {
            if unknown_tint {
                r.acceptance = overlapping().0.acceptance;
                r.identity.measurement_sha256 = crate::digest(&r.acceptance).unwrap();
                p.outputs.get_mut("output").unwrap()[0].paint = json!({"mode":"output_ink"});
            }
            let mut report = crate::begin(&r).unwrap();
            check(&r, &p, &c, &mut report).unwrap();
            assert_eq!(report.findings.iter().find(|f| f.obligation.key == "rule/arbitrary/paint").unwrap().status, Status::Incomplete);
        }
    }
    #[test]
    fn wrong_paint_on_a_merged_layer_is_not_automatically_blame_for_every_rule() {
        let bytes = fixture("1 0 0 rg 0 0 20 10 re f");
        let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        crate::ordering::try_reconcile(&bytes, &mut c).unwrap();
        assert!(c.paint_order_verified);
        let mut r = crate::attribution::tests::request();
        let mut rule = r.recipe.rules[0].clone();
        rule.rule_key = "second".into();
        r.recipe.rules.push(rule);
        let mut link = r.recipe.file_links[0].clone();
        link.key = "second-output".into();
        link.source_rule = "second".into();
        r.recipe.file_links.push(link);
        let mut program = expected();
        program.outputs.insert("second-output".into(), program.outputs["output"].clone());
        let mut report = crate::begin(&r).unwrap();
        check(&r, &program, &c, &mut report).unwrap();
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "global/colour").unwrap().status, Status::Fail);
        for key in ["rule/arbitrary/paint", "rule/second/paint"] {
            assert_eq!(report.findings.iter().find(|f| f.obligation.key == key).unwrap().status, Status::Incomplete);
        }
    }
}
