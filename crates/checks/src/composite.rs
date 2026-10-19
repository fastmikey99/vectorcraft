//! Positive support of an opaque composite destination. Ownership and expected
//! non-erasing pieces are established before observing the exported ink coverage.
use crate::{
    Request,
    attribution::Plan,
    coverage,
    feature_offset::WorkBudget,
    pdf::Census,
    preparation::{Applicability, select},
    program::{Piece, Program},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use vectorcraft_geom::Shape;

fn tones(piece: &Piece) -> Result<BTreeMap<String, f64>, String> {
    let result = match piece.paint.get("mode").and_then(Value::as_str) {
        Some("cmyk") => {
            let values = piece.paint.get("value").and_then(Value::as_array).filter(|v| v.len() == 4).ok_or("Expected CMYK needs four components")?;
            ["Cyan", "Magenta", "Yellow", "Black"]
                .into_iter()
                .zip(values)
                .map(|(key, v)| {
                    v.as_f64()
                        .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
                        .map(|n| (key.into(), n / 100.0))
                        .ok_or("Invalid expected CMYK component".to_string())
                })
                .collect::<Result<_, _>>()?
        }
        Some("output_ink") if piece.ink.is_some() => crate::tone::tones(&piece.paint, piece.ink.as_deref())?,
        _ if piece.paint.get("type").and_then(Value::as_str) == Some("solid") => crate::tone::tones(&piece.paint, piece.ink.as_deref())?,
        _ => return Err("Composite positive-support requires resolved solid expected paint".into()),
    };
    crate::tone::positive_supported(&result)?;
    Ok(result)
}
fn active_plates(request: &Request) -> Result<Vec<&Value>, String> {
    Ok(request
        .recipe
        .foundation
        .as_ref()
        .and_then(|f| f.pointer("/verification/plates"))
        .and_then(Value::as_array)
        .ok_or("Physical plates missing")?
        .iter()
        .filter(|p| p.get("garment").and_then(Value::as_str).is_some_and(|g| request.garment.matches(g)))
        .collect())
}
fn link_keys(plate: &Value) -> Result<Vec<&str>, String> {
    plate.get("links").and_then(Value::as_array).ok_or("Plate links missing")?.iter().map(|v| v.as_str().ok_or("Plate link invalid".into())).collect()
}
fn claims(request: &Request, program: &Program, budget: &mut WorkBudget) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let mut claims: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for plate in active_plates(request)? {
        let key = plate.get("key").and_then(Value::as_str).ok_or("Physical plate key missing")?;
        // Exact ownership remains declared even when the expected contribution is empty.
        if plate.get("name_mode").and_then(Value::as_str) == Some("exact") {
            let ink = plate.get("name").and_then(Value::as_str).ok_or("Exact channel missing")?;
            claims.entry(ink.into()).or_default().insert(key.into());
        }
        for link in link_keys(plate)? {
            for piece in program
                .outputs
                .get(link)
                .ok_or_else(|| format!("Plate {key}, link {link}: complete expected output required for channel ownership"))?
            {
                budget.charge(piece.regions.len().saturating_add(1))?;
                for channel in tones(piece).map_err(|e| format!("{link}: {e}"))?.keys() {
                    claims.entry(channel.clone()).or_default().insert(key.into());
                }
            }
        }
    }
    Ok(claims)
}
/// A known shared channel must not receive an exact-plate Pass either. Unknown
/// foreign paints do not weaken the existing exact-channel reconstruction gate.
pub(crate) fn exact_collision(request: &Request, program: &Program, key: &str, ink: &str, budget: &mut WorkBudget) -> Result<bool, String> {
    for plate in active_plates(request)? {
        if plate.get("key").and_then(Value::as_str) == Some(key) {
            continue;
        }
        if plate.get("name_mode").and_then(Value::as_str) == Some("exact") && plate.get("name").and_then(Value::as_str) == Some(ink) {
            return Ok(true);
        }
        for link in link_keys(plate)? {
            for piece in program.outputs.get(link).into_iter().flatten() {
                budget.charge(piece.regions.len().saturating_add(1))?;
                if tones(piece).is_ok_and(|tones| tones.contains_key(ink)) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn disjoint(a: &Piece, b: &Piece) -> bool {
    if a.page != b.page || a.path.is_empty() || b.path.is_empty() {
        return true;
    }
    let x = a.path.to_bezpath().bounding_box();
    let y = b.path.to_bezpath().bounding_box();
    let e = a.construction_error_mm + b.construction_error_mm;
    e.is_finite() && e >= 0.0 && (x.x1 + e < y.x0 || y.x1 + e < x.x0 || x.y1 + e < y.y0 || y.y1 + e < x.y0)
}
pub(crate) fn reconstruct(
    request: &Request,
    plan: &Plan,
    program: &Program,
    census: &Census,
    plate: &Value,
    budget: &mut WorkBudget,
) -> Result<BTreeMap<usize, coverage::Coverage>, String> {
    coverage::reconciled_vector_census(census)?;
    if !plan.issues.is_empty() {
        return Err("Composite positive-support requires fully resolved source assignments".into());
    }
    let key = plate.get("key").and_then(Value::as_str).ok_or("Composite plate key missing")?;
    let destination = plate.get("destination").and_then(Value::as_str).ok_or("Composite destination missing")?;
    let keys = link_keys(plate)?;
    let claims = claims(request, program, budget)?;
    if let Some((ink, owners)) = claims.iter().find(|(_, owners)| owners.len() != 1) {
        return Err(format!("Composite channel {ink} has ambiguous physical plate ownership: {owners:?}"));
    }
    let owned: BTreeSet<_> = claims.iter().filter(|(_, owners)| owners.contains(key)).map(|(ink, _)| ink.clone()).collect();
    let pieces: Vec<_> = keys.iter().flat_map(|l| program.outputs.get(*l).into_iter().flatten()).collect();
    let foundation = request.recipe.foundation.as_ref().ok_or("Foundation missing")?;
    for piece in &pieces {
        budget.charge(piece.regions.len().saturating_add(1))?;
        if !tones(piece)?.iter().any(|(ink, tone)| owned.contains(ink) && *tone > 0.0) {
            return Err(format!("Composite piece {:?} has no positive owned ink; expected zero/white paint order is unresolved", piece.regions));
        }
        if piece.paint.get("type").and_then(Value::as_str) == Some("solid") {
            for region in &piece.regions {
                let facts = plan.facts.get(region).ok_or("Composite source facts missing")?;
                let mut matching = vec![];
                for route in foundation.get("input_routes").and_then(Value::as_array).ok_or("Input routes missing")? {
                    budget.charge(1)?;
                    match select(route.get("when").unwrap_or(&Value::Null), facts) {
                        Applicability::Unknown => return Err(format!("{region}: composite source route unresolved")),
                        Applicability::Applies => matching.push(route),
                        Applicability::DoesNotApply => {}
                    }
                }
                if matching.len() != 1 || matching.first().and_then(|r| r.get("destination")).and_then(Value::as_str) != Some(destination) {
                    return Err(format!("{region}: source route disagrees with composite destination {destination}"));
                }
            }
        }
    }
    let links: Vec<_> = request.recipe.file_links.iter().filter(|l| request.garment.matches(&l.garment)).collect();
    let own_links: Vec<_> = links.iter().filter(|l| keys.contains(&l.key.as_str())).collect();
    for foreign in links.iter().filter(|l| !keys.contains(&l.key.as_str())) {
        let foreign_pieces = program.outputs.get(&foreign.key).ok_or("Foreign contribution unresolved")?;
        if foreign.name_mode == "exact" && foreign.overprint {
            continue;
        }
        if own_links.iter().all(|l| foreign.layer_order < l.layer_order) {
            continue;
        }
        let mut separate = true;
        'pairs: for a in foreign_pieces {
            for b in &pieces {
                budget.charge(a.path.anchor_count().saturating_add(b.path.anchor_count()).saturating_add(1))?;
                if !disjoint(a, b) {
                    separate = false;
                    break 'pairs;
                }
            }
        }
        if separate {
            continue;
        }
        return Err(format!("{}: foreign paint can erase composite support without a bound safe ordering", foreign.key));
    }
    let mut pages: BTreeSet<_> = pieces.iter().map(|p| p.page).collect();
    for paint in &census.paints {
        budget.charge(1)?;
        let source = paint.source_space.and_then(|i| census.paint_sources.get(i)).ok_or("Observed source identity missing")?;
        let tones = crate::tone::source_tones(paint, source)?;
        crate::tone::positive_supported(&tones)?;
        if let Some(ink) = tones.keys().find(|ink| !claims.contains_key(*ink)) {
            return Err(format!("Observed channel {ink} has no expected physical owner"));
        }
        if tones.keys().any(|ink| owned.contains(ink)) {
            pages.insert(paint.page.ok_or("Observed composite page missing")?);
        }
    }
    if pages.is_empty() {
        pages.insert(0);
    }
    let mut result = BTreeMap::new();
    for page in pages {
        let parts = owned.iter().map(|ink| coverage::positive_with_budget(census, page, ink, budget)).collect::<Result<Vec<_>, _>>()?;
        result.insert(page, coverage::aggregate(parts, budget)?);
    }
    Ok(result)
}
