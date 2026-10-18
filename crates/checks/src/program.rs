//! Execute the versioned construction graph against approved preparation geometry.
//! Expected geometry is never inferred from the final output or a compiler-filtered plate list.
use crate::{Request, attribution::Plan, digest, preparation::Applicability, preparation::select};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use vectorcraft_geom::{FillRule, PathData};
use vectorcraft_pathops::BoolOp;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionGeometry {
    /// Original source mask; ordered preparation edits are verified by fresh replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_mask: Option<crate::prepared_mask::Mask>,
    pub page: usize,
    pub path_mm: PathData,
    pub fill_rule: FillRule,
    /// Original source paint; assignment policies define any authorised replacement.
    pub paint: Value,
    #[serde(default, skip_serializing_if = "zero_error")]
    pub geometry_error_mm: f64,
    /// Raw original PDF paint order, only when the complete path census reconciles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_order: Option<usize>,
}
fn zero_error(n: &f64) -> bool {
    *n == 0.0
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inputs {
    pub regions: BTreeMap<String, RegionGeometry>,
}
#[derive(Clone, Debug)]
pub struct Piece {
    pub page: usize,
    pub path: PathData,
    pub regions: BTreeSet<String>,
    pub ink: Option<String>,
    pub paint: Value,
    /// Error budget of expectation construction, distinct from production tolerance.
    pub construction_error_mm: f64,
}
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub rules: BTreeMap<String, Vec<Piece>>,
    pub outputs: BTreeMap<String, Vec<Piece>>,
    pub unresolved: BTreeMap<String, String>,
}
fn boolean(a: &PathData, b: &PathData, op: BoolOp) -> Result<PathData, String> {
    vectorcraft_pathops::try_boolean_for_measurement(a, FillRule::NonZero, b, FillRule::NonZero, op).map_err(|e| e.to_string())
}
pub fn union(paths: impl IntoIterator<Item = PathData>) -> Result<PathData, String> {
    let mut out = PathData::default();
    for p in paths {
        out = if out.is_empty() { p } else { boolean(&out, &p, BoolOp::Union)? };
    }
    Ok(out)
}
fn source(request: &Request, plan: &Plan, inputs: &Inputs, program: &Program, sources: &[crate::recipe::Source]) -> Result<Vec<Piece>, String> {
    let f = request.recipe.foundation.as_ref().ok_or("Foundation missing")?;
    let mut pieces = vec![];
    for s in sources {
        if s.kind == "rule" {
            pieces.extend(program.rules.get(&s.key).ok_or_else(|| format!("Dependency {} unresolved", s.key))?.clone());
            continue;
        }
        let selection = f.get("selections").and_then(|f| f.get(&s.key)).ok_or("Selection missing")?;
        for (id, geometry) in &inputs.regions {
            let facts = plan.facts.get(id).ok_or("Region facts missing")?;
            match select(selection, facts) {
                Applicability::DoesNotApply => continue,
                Applicability::Unknown => return Err(format!("{id}: source facts unresolved")),
                Applicability::Applies => {}
            }
            let path = vectorcraft_pathops::try_boolean_for_measurement(
                &geometry.path_mm,
                geometry.fill_rule,
                &PathData::default(),
                FillRule::NonZero,
                BoolOp::Union,
            )
            .map_err(|e| e.to_string())?;
            pieces.push(Piece {
                page: geometry.page,
                path,
                regions: BTreeSet::from([id.clone()]),
                ink: facts.get("ink").and_then(|f| f.value.as_str()).map(String::from),
                paint: plan.paints.get(id).cloned().unwrap_or_else(|| geometry.paint.clone()),
                construction_error_mm: 0.00001 + geometry.geometry_error_mm,
            });
        }
    }
    Ok(pieces)
}
pub fn execute(request: &Request, plan: &Plan) -> Result<Program, String> {
    let inputs = request.geometry.as_ref().ok_or("Prepared geometry artifacts are missing")?;
    let baseline = request.baseline.as_ref().ok_or("Baseline missing")?;
    if inputs.regions.len() != baseline.regions.len() || inputs.regions.len() > 50_000 {
        return Err("Prepared region geometry does not account for the complete baseline".into());
    }
    for region in &baseline.regions {
        let geometry = inputs.regions.get(&region.id).ok_or("Prepared region geometry missing")?;
        match (&geometry.prepared_mask, &region.prepared_mask_sha256) {
            (Some(mask), Some(hash)) if digest(mask)? == *hash => {
                mask.validate()?;
            }
            (None, None) => {}
            _ => return Err(format!("{}: prepared raster mask identity mismatch", region.id)),
        }
        if !geometry.geometry_error_mm.is_finite() || geometry.geometry_error_mm < 0.0 {
            return Err("Prepared geometry numerical error is invalid".into());
        }
        if digest(geometry)? != region.geometry_sha256 {
            return Err(format!("{}: prepared geometry identity mismatch", region.id));
        }
        if !baseline.selected_pages.contains(&geometry.page) || geometry.path_mm.anchor_count() > 100_000 {
            return Err("Prepared geometry page/size invalid".into());
        }
    }
    let f = request.recipe.foundation.as_ref().ok_or("Foundation missing")?;
    let mut program = Program::default();
    let mut pending: Vec<_> = request.recipe.rules.iter().filter(|r| r.operation != "policy" && request.garment.matches(&r.garment)).collect();
    while !pending.is_empty() {
        let before = pending.len();
        pending.retain(|r| {
            let t = f.get("treatments").and_then(|t| t.get(&r.rule_key)).unwrap_or(&Value::Null);
            let neighbours: Vec<crate::recipe::Source> =
                serde_json::from_value(t.get("neighbours").cloned().unwrap_or(Value::Null)).unwrap_or_default();
            if r.sources
                .iter()
                .chain(&r.protection)
                .chain(&neighbours)
                .any(|s| s.kind == "rule" && !program.rules.contains_key(&s.key) && !program.unresolved.contains_key(&s.key))
            {
                return true;
            }
            let result = (|| -> Result<Vec<Piece>, String> {
                if plan.rules.get(&r.rule_key).is_none_or(|s| !s.unresolved.is_empty()) {
                    return Err("Rule source applicability unresolved".into());
                }
                let mut pieces = source(request, plan, inputs, &program, &r.sources)?;
                let selected = &plan.rules.get(&r.rule_key).ok_or("Rule selection missing")?.regions;
                pieces.retain(|p| p.regions.iter().any(|id| selected.contains(id)));
                let mut grouped: BTreeMap<(usize, String), Vec<Piece>> = BTreeMap::new();
                for (i, p) in pieces.into_iter().enumerate() {
                    let partition = t.get("partition").and_then(Value::as_str).ok_or("Partition missing")?;
                    let group = match partition {
                        "union" => String::new(),
                        "ink" => p.ink.clone().ok_or("Exact ink assignment missing")?,
                        "object" => format!("{i}"),
                        _ => return Err("Unknown partition".into()),
                    };
                    grouped.entry((p.page, group)).or_default().push(p);
                }
                let protected = source(request, plan, inputs, &program, &r.protection)?;
                let neighbours = source(request, plan, inputs, &program, &neighbours)?;
                let mut out = vec![];
                for ((page, _), parts) in grouped {
                    let first = parts.first().ok_or("Empty partition")?;
                    let protection_error = protected.iter().filter(|p| p.page == page).map(|p| p.construction_error_mm).fold(0.0, f64::max);
                    if t.pointer("/paint/mode").and_then(Value::as_str) == Some("source")
                        && parts.iter().any(|p| p.paint != first.paint || p.ink != first.ink)
                    {
                        if !matches!(r.operation.as_str(), "preserve" | "combine") {
                            return Err("Extending distinct source paints requires a qualified paint-extension policy".into());
                        }
                        // The physical output is still the union, but colour fidelity needs
                        // each original painted region. A union cannot inherit the first colour.
                        let mask = union(protected.iter().filter(|p| p.page == page).map(|p| p.path.clone()))?;
                        for mut piece in parts {
                            if !mask.is_empty() {
                                piece.path = boolean(&piece.path, &mask, BoolOp::Difference)?;
                                piece.construction_error_mm += protection_error;
                            }
                            out.push(piece);
                        }
                        continue;
                    }
                    let original = union(parts.iter().map(|p| p.path.clone()))?;
                    let mut path = original.clone();
                    let mut error = parts.iter().map(|p| p.construction_error_mm).fold(0.0, f64::max);
                    match r.operation.as_str() {
                        "preserve" | "combine" => {}
                        "spread" | "choke" => {
                            let distance = r.distance_mm.ok_or("Construction distance missing")?;
                            if r.join.as_deref() != Some("round") {
                                return Err("Round join required".into());
                            }
                            let edges = t.get("edges").and_then(Value::as_str).ok_or("Edges missing")?;
                            if edges == "all" {
                                path = vectorcraft_pathops::try_offset_path_for_measurement(
                                    &path,
                                    if r.operation == "choke" { -distance } else { distance },
                                    0.00001,
                                )
                                .map_err(|e| e.to_string())?;
                            } else {
                                let neighbour = union(neighbours.iter().filter(|p| p.page == page).map(|p| p.path.clone()))?;
                                let edges = crate::geometry::selected_edges(&path, &neighbour, edges == "shared_only", 0.00001)?;
                                if !edges.is_empty() {
                                    let band = {
                                        let stroke = vectorcraft_geom::kurbo::Stroke::new(2.0 * distance)
                                            .with_caps(vectorcraft_geom::kurbo::Cap::Round)
                                            .with_join(vectorcraft_geom::kurbo::Join::Round);
                                        let raw = PathData::from_bezpath(&vectorcraft_geom::kurbo::stroke(
                                            edges.to_bezpath().iter(),
                                            &stroke,
                                            &Default::default(),
                                            0.00001,
                                        ));
                                        boolean(&raw, &PathData::default(), BoolOp::Union)?
                                    };
                                    if band.is_empty() && distance > 0.0 {
                                        return Err("Selected-edge construction failed".into());
                                    }
                                    path = boolean(&path, &band, if r.operation == "choke" { BoolOp::Difference } else { BoolOp::Union })?;
                                }
                            }
                            error += 0.00002;
                            if t.get("result").and_then(Value::as_str) == Some("outer_band") {
                                path = boolean(&path, &original, BoolOp::Difference)?;
                            }
                        }
                        _ => return Err("Unknown shape operation".into()),
                    }
                    if !protected.is_empty() {
                        let mask = union(protected.iter().filter(|p| p.page == page).map(|p| p.path.clone()))?;
                        path = boolean(&path, &mask, BoolOp::Difference)?;
                        error += protection_error;
                    }
                    let paint = t.get("paint").ok_or("Expected paint missing")?;
                    out.push(Piece {
                        page,
                        path,
                        regions: parts.iter().flat_map(|p| p.regions.clone()).collect(),
                        ink: first.ink.clone(),
                        paint: if paint.get("mode").and_then(Value::as_str) == Some("source") { first.paint.clone() } else { paint.clone() },
                        construction_error_mm: error,
                    });
                }
                Ok(out)
            })();
            match result {
                Ok(pieces) => {
                    program.rules.insert(r.rule_key.clone(), pieces);
                }
                Err(reason) => {
                    program.unresolved.insert(r.rule_key.clone(), reason);
                }
            }
            false
        });
        if pending.len() == before {
            return Err("Geometry dependency graph cannot progress".into());
        }
    }
    for l in request.recipe.file_links.iter().filter(|l| request.garment.matches(&l.garment)) {
        if let Some(pieces) = program.rules.get(&l.source_rule) {
            let matching =
                f.get("outputs").and_then(|o| o.get(&l.key)).and_then(|o| o.get("select")).and_then(Value::as_str) == Some("matching_output_ink");
            let pieces = pieces
                .iter()
                .filter(|p| !matching || p.ink == l.separation_name)
                .cloned()
                .map(|mut p| {
                    if p.paint.get("mode").and_then(Value::as_str) == Some("output_ink") {
                        p.ink = l.separation_name.clone();
                    }
                    p
                })
                .collect();
            program.outputs.insert(l.key.clone(), pieces);
        }
    }
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_geom::{BezPath, Point, Shape};
    fn rectangle(x: f64, y: f64, w: f64, h: f64) -> PathData {
        let mut p = BezPath::new();
        p.move_to((x, y));
        p.line_to((x + w, y));
        p.line_to((x + w, y + h));
        p.line_to((x, y + h));
        p.close_path();
        PathData::from_bezpath(&p)
    }
    fn request() -> Request {
        let mut r = crate::attribution::tests::request();
        let geometry = RegionGeometry {
            prepared_mask: None,
            page: 0,
            path_mm: rectangle(0.0, 0.0, 10.0, 10.0),
            fill_rule: FillRule::NonZero,
            paint: serde_json::json!({"mode":"cmyk","value":[0,100,0,0]}),
            geometry_error_mm: 0.0,
            source_order: None,
        };
        r.baseline.as_mut().unwrap().regions[0].geometry_sha256 = digest(&geometry).unwrap();
        r.geometry = Some(Inputs { regions: BTreeMap::from([("region-1".into(), geometry)]) });
        refresh(&mut r);
        r
    }
    fn refresh(r: &mut Request) {
        r.identity.baseline_sha256 = digest(r.baseline.as_ref().unwrap()).unwrap();
        r.identity.recipe_sha256 = digest(&r.recipe).unwrap();
    }
    #[test]
    fn rounded_spread_uses_physical_mm_and_not_a_square_corner() {
        let r = request();
        let plan = crate::attribution::resolve(&r).unwrap();
        let p = execute(&r, &plan).unwrap();
        assert!(p.unresolved.is_empty(), "{:?}", p.unresolved);
        let path = &p.outputs["output"][0].path.to_bezpath();
        let bounds = path.bounding_box();
        assert!((bounds.x0 + 0.2).abs() < 0.0001);
        assert!((bounds.x1 - 10.2).abs() < 0.0001);
        assert_eq!(path.winding(Point::new(-0.19, -0.19)), 0);
        assert_ne!(path.winding(Point::new(-0.1, -0.1)), 0);
    }
    #[test]
    fn chained_offsets_use_completed_dependencies_without_process_dispatch() {
        let mut r = request();
        let original = r.recipe.rules[0].clone();
        for (key, parent) in [("second", "arbitrary"), ("third", "second")] {
            let mut rule = original.clone();
            rule.rule_key = key.into();
            rule.sources = vec![crate::recipe::Source { kind: "rule".into(), key: parent.into() }];
            r.recipe.rules.push(rule);
            let treatment = r.recipe.foundation.as_ref().unwrap()["treatments"]["arbitrary"].clone();
            r.recipe.foundation.as_mut().unwrap()["treatments"][key] = treatment;
        }
        r.recipe.process = "Any renamed production family".into();
        refresh(&mut r);
        let plan = crate::attribution::resolve(&r).unwrap();
        let p = execute(&r, &plan).unwrap();
        assert!(p.unresolved.is_empty(), "{:?}", p.unresolved);
        let bounds = p.rules["third"][0].path.to_bezpath().bounding_box();
        assert!((bounds.x0 + 0.6).abs() < 0.0001);
        assert!((bounds.x1 - 10.6).abs() < 0.0001);
    }
    #[test]
    fn measurement_boolean_retains_a_tiny_real_island() {
        let big = rectangle(0.0, 0.0, 10.0, 10.0);
        let tiny = rectangle(11.0, 5.0, 0.0001, 0.0001);
        let combined = union([big, tiny]).unwrap();
        assert_eq!(combined.subpaths.len(), 2);
        assert_ne!(combined.to_bezpath().winding(Point::new(11.00005, 5.00005)), 0);
    }
    #[test]
    fn contact_selection_does_not_treat_a_nearby_edge_as_shared() {
        let a = rectangle(0.0, 0.0, 10.0, 10.0);
        let neighbour = rectangle(10.0, 2.0, 5.0, 6.0);
        let selected = crate::geometry::selected_edges(&a, &neighbour, true, 0.000001).unwrap();
        assert_eq!(selected.subpaths.len(), 1);
        let bounds = selected.to_bezpath().bounding_box();
        assert_eq!(bounds.x0, 10.0);
        assert_eq!(bounds.y0, 2.0);
        assert_eq!(bounds.y1, 8.0);
        assert!(crate::geometry::selected_edges(&a, &rectangle(10.01, 2.0, 5.0, 6.0), true, 0.000001).unwrap().is_empty());
    }
    #[test]
    fn coincident_edges_remain_closed_at_fractional_pdf_coordinates() {
        for edges in ["shared_only", "except_shared"] {
            let mut r = request();
            let g = r.geometry.as_mut().unwrap().regions.get_mut("region-1").unwrap();
            g.path_mm = rectangle(3.5277777777777777, 24.694444444444443, 7.0555555555555545, 7.055555555555553);
            r.baseline.as_mut().unwrap().regions[0].geometry_sha256 = digest(g).unwrap();
            let t = &mut r.recipe.foundation.as_mut().unwrap()["treatments"]["arbitrary"];
            t["edges"] = serde_json::json!(edges);
            t["neighbours"] = serde_json::json!([{"kind":"artwork","key":"selected"}]);
            refresh(&mut r);
            let program = execute(&r, &crate::attribution::resolve(&r).unwrap()).unwrap();
            assert!(program.unresolved.is_empty(), "{:?}", program.unresolved);
            let path = &program.outputs["output"][0].path;
            assert!(crate::geometry::Boundary::new(path, FillRule::NonZero, 0.001).is_ok(), "{edges}: {}", path.to_bezpath().to_svg());
            if edges == "except_shared" {
                let original = &r.geometry.as_ref().unwrap().regions["region-1"].path_mm;
                assert_eq!(crate::geometry::compare(original, path, 0.0001, 0.00001).unwrap().status, crate::Status::Pass);
            }
        }
    }
    #[test]
    fn prepared_geometry_is_bound_to_the_approved_baseline() {
        let mut r = request();
        r.geometry.as_mut().unwrap().regions.get_mut("region-1").unwrap().path_mm = rectangle(0.0, 0.0, 11.0, 10.0);
        let p = crate::attribution::resolve(&r).unwrap();
        assert!(execute(&r, &p).unwrap_err().contains("identity mismatch"));
    }
    #[test]
    fn protected_edges_retain_the_masks_construction_uncertainty() {
        let mut r = request();
        let geometry = r.geometry.as_mut().unwrap().regions.get_mut("region-1").unwrap();
        geometry.geometry_error_mm = 0.01;
        r.baseline.as_mut().unwrap().regions[0].geometry_sha256 = digest(geometry).unwrap();
        refresh(&mut r);
        let plain = execute(&r, &crate::attribution::resolve(&r).unwrap()).unwrap();
        r.recipe.rules[0].protection = r.recipe.rules[0].sources.clone();
        r.recipe.foundation.as_mut().unwrap()["treatments"]["arbitrary"]["protection"] = serde_json::json!("exclude");
        refresh(&mut r);
        let protected = execute(&r, &crate::attribution::resolve(&r).unwrap()).unwrap();
        let added = protected.outputs["output"][0].construction_error_mm - plain.outputs["output"][0].construction_error_mm;
        assert!((added - 0.01001).abs() < 1e-12);
    }
    #[test]
    fn combining_source_colours_retains_each_paint_and_rejects_ambiguous_extension() {
        let mut r = request();
        let mut geometry = r.geometry.as_ref().unwrap().regions["region-1"].clone();
        geometry.path_mm = rectangle(20.0, 0.0, 10.0, 10.0);
        geometry.paint = serde_json::json!({"mode":"cmyk","value":[100,0,0,0]});
        let baseline = r.baseline.as_mut().unwrap();
        let mut region = baseline.regions[0].clone();
        region.id = "region-2".into();
        region.source_objects = vec!["original-path-2".into()];
        region.geometry_sha256 = digest(&geometry).unwrap();
        baseline.source_census.push("original-path-2".into());
        baseline.regions.push(region);
        r.geometry.as_mut().unwrap().regions.insert("region-2".into(), geometry);
        r.recipe.foundation.as_mut().unwrap()["treatments"]["arbitrary"]["paint"] = serde_json::json!({"mode":"source"});
        r.recipe.foundation.as_mut().unwrap()["treatments"]["arbitrary"]["partition"] = serde_json::json!("union");
        for operation in ["preserve", "combine"] {
            r.recipe.rules[0].operation = operation.into();
            r.recipe.rules[0].distance_mm = None;
            r.recipe.rules[0].join = None;
            refresh(&mut r);
            let plan = crate::attribution::resolve(&r).unwrap();
            let result = execute(&r, &plan).unwrap();
            assert!(result.unresolved.is_empty(), "{:?}", result.unresolved);
            let pieces = &result.outputs["output"];
            assert_eq!(pieces.len(), 2);
            assert_ne!(pieces[0].paint, pieces[1].paint);
            assert_eq!(union(pieces.iter().map(|p| p.path.clone())).unwrap().subpaths.len(), 2);
        }
        r.recipe.rules[0].operation = "spread".into();
        r.recipe.rules[0].distance_mm = Some(0.2);
        r.recipe.rules[0].join = Some("round".into());
        refresh(&mut r);
        let plan = crate::attribution::resolve(&r).unwrap();
        assert!(execute(&r, &plan).unwrap().unresolved["arbitrary"].contains("paint-extension policy"));
    }
}
