//! Independent per-ink tonal reconciliation. No binary zero/nonzero mask or area suppression.
//! Solid device CMYK/grey and named-spot paints are predicted in paint order. Unsupported
//! source colour spaces, groups, images and masks remain explicit rather than RGB-flattened.
use crate::{Status, geometry::Boundary, independent::Render, pdf::Census};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use vectorcraft_geom::{FillRule, Point};
#[derive(Clone, Debug, Serialize)]
pub struct PlaneComparison {
    pub page: u32,
    pub name: String,
    pub status: Status,
    pub compared_pixels: u64,
    pub boundary_pixels: u64,
    pub wrong_pixels: u64,
    pub maximum_tone_error: f64,
    pub first_defect_mm: Option<[f64; 2]>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Comparison {
    pub status: Status,
    pub uncertainty_mm: f64,
    pub planes: Vec<PlaneComparison>,
    pub issues: Vec<String>,
}
struct Paint {
    page: usize,
    boundary: Boundary,
    inks: BTreeMap<String, f64>,
    overprint: bool,
    skip_zero_process: bool,
}
fn tones(paint: &Value, ink: Option<&str>) -> Result<BTreeMap<String, f64>, String> {
    let tint = paint.get("tint").and_then(Value::as_f64).unwrap_or(1.0);
    if let Some(ink) = ink {
        if ["All", "None", "Cyan", "Magenta", "Yellow", "Black"].contains(&ink) {
            return Err("Special/process spot-name collision requires explicit PDF reconciliation".into());
        }
        return Ok(BTreeMap::from([(ink.into(), tint)]));
    }
    let color = paint.get("color").ok_or("Solid source colour missing")?;
    let component = |key: &str| color.get(key).and_then(Value::as_f64).filter(|n| (0.0..=1.0).contains(n)).ok_or("Invalid source component");
    let values = match color.get("model").and_then(Value::as_str) {
        Some("cmyk") => [component("c")?, component("m")?, component("y")?, component("k")?],
        Some("gray") => [0.0, 0.0, 0.0, component("k")?],
        _ => return Err("Source colour/profile conversion needs a qualified prediction".into()),
    };
    Ok(["Cyan", "Magenta", "Yellow", "Black"].into_iter().zip(values).map(|(k, v)| (k.into(), v)).collect())
}
pub fn compare(census: &Census, render: &Render, tolerance_mm: Option<f64>) -> Result<Comparison, String> {
    let uncertainty_mm = render.pixel_pitch_mm * 2.0f64.sqrt() + 0.000002 + census.paints.iter().map(|p| p.geometry_error_mm).fold(0.0, f64::max);
    let mut issues = census.unresolved.clone();
    issues.extend(render.diagnostics.clone());
    if census.budget_exhausted {
        issues.push("Native census budget exhausted".into());
    }
    if tolerance_mm.is_none_or(|t| !t.is_finite() || t < uncertainty_mm) {
        issues.push("Independent raster resolution does not resolve approved geometric acceptance".into());
    }
    let mut paints = vec![];
    let mut inks: BTreeSet<String> = ["Cyan", "Magenta", "Yellow", "Black"].into_iter().map(String::from).collect();
    for p in &census.paints {
        let (Some(page), Some(path)) = (p.page, p.geometry_mm.as_ref()) else {
            issues.push(format!("{}: full print coverage unavailable", p.id));
            continue;
        };
        match (Boundary::new(path, FillRule::NonZero, 0.000001), tones(&p.paint, p.ink.as_deref())) {
            (Ok(boundary), Ok(tones)) => {
                inks.extend(tones.keys().cloned());
                let device_cmyk = p.ink.is_none() && p.paint.get("color").and_then(|c| c.get("model")).and_then(Value::as_str) == Some("cmyk");
                paints.push(Paint { page, boundary, inks: tones, overprint: p.overprint, skip_zero_process: device_cmyk && !p.overprint_zero });
            }
            (a, b) => {
                if let Err(e) = a {
                    issues.push(e);
                }
                if let Err(e) = b {
                    issues.push(e);
                }
            }
        }
    }
    let inventory: BTreeSet<_> = render.planes.iter().map(|p| (p.page, p.name.clone())).collect();
    if inventory.len() != render.planes.len() {
        issues.push("Duplicate independent colourant planes".into());
    }
    for page in 0..census.pages {
        for name in &inks {
            if !inventory.contains(&(page as u32 + 1, name.clone())) {
                issues.push(format!("Page {}: missing {name:?} plane", page + 1));
            }
        }
    }
    for p in &render.planes {
        if p.page == 0 || p.page as usize > census.pages || !inks.contains(&p.name) {
            issues.push(format!("Unexpected page/ink: {} {:?}", p.page, p.name));
        }
        if let Some([w, h]) = p.page.checked_sub(1).and_then(|n| census.page_sizes_mm.get(n as usize))
            && ((w / render.pixel_pitch_mm - f64::from(p.width)).abs() > 1.0 || (h / render.pixel_pitch_mm - f64::from(p.height)).abs() > 1.0)
        {
            issues.push(format!("Page {}: independent crop/scale dimensions disagree", p.page));
        }
    }
    if !issues.is_empty() {
        return Ok(Comparison { status: Status::Incomplete, uncertainty_mm, planes: vec![], issues });
    }
    let mut results = vec![];
    let mut budget = 0usize;
    let mut pixels = 0u64;
    for p in &render.planes {
        let bytes = std::fs::read(&p.path).map_err(|e| e.to_string())?;
        if crate::sha256(&bytes) != p.sha256 {
            return Err("Independent plane changed before measurement".into());
        }
        let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?.into_luma8();
        if image.dimensions() != (p.width, p.height) {
            return Err("Independent plane dimensions changed".into());
        }
        let mut result = PlaneComparison {
            page: p.page,
            name: p.name.clone(),
            status: Status::Pass,
            compared_pixels: 0,
            boundary_pixels: 0,
            wrong_pixels: 0,
            maximum_tone_error: 0.0,
            first_defect_mm: None,
        };
        for (x, y, pixel) in image.enumerate_pixels() {
            pixels += 1;
            if pixels > 20_000_000 {
                return Err("Independent tonal comparison pixel budget exceeded".into());
            }
            let point = Point::new((x as f64 + 0.5) * render.pixel_pitch_mm, (y as f64 + 0.5) * render.pixel_pitch_mm);
            let mut expected = 0.0;
            let mut uncertain = false;
            for paint in paints.iter().filter(|a| a.page + 1 == p.page as usize) {
                if paint.boundary.near_boundary(point, uncertainty_mm, &mut budget)? {
                    uncertain = true;
                }
                if paint.boundary.contains(point, FillRule::NonZero) {
                    // ISO 32000-1 §8.6.7 and Table 148: OP preserves unspecified
                    // colourants. Only direct DeviceCMYK with OPM 1 preserves zero
                    // components; a zero Separation tint still paints its named ink.
                    if !paint.overprint {
                        expected = paint.inks.get(&p.name).copied().unwrap_or(0.0);
                    } else if let Some(&tone) = paint.inks.get(&p.name)
                        && !(paint.skip_zero_process && tone == 0.0)
                    {
                        expected = tone;
                    }
                }
            }
            if uncertain {
                result.boundary_pixels += 1;
                continue;
            }
            result.compared_pixels += 1;
            let observed = 1.0 - f64::from(pixel.0[0]) / 255.0;
            let error = (observed - expected).abs();
            result.maximum_tone_error = result.maximum_tone_error.max(error);
            // Eight-bit quantisation is an engineering bound, not a production tint tolerance.
            if error > 1.0 / 255.0 + f64::from(f32::EPSILON) {
                result.status = Status::Fail;
                result.wrong_pixels += 1;
                result.first_defect_mm.get_or_insert([point.x, point.y]);
            }
        }
        if result.compared_pixels == 0 {
            result.status = Status::Incomplete;
        }
        results.push(result);
    }
    let status = if results.iter().any(|p| p.status == Status::Fail) {
        Status::Fail
    } else if results.iter().any(|p| p.status == Status::Incomplete) {
        Status::Incomplete
    } else {
        Status::Pass
    };
    Ok(Comparison { status, uncertainty_mm, planes: results, issues })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tones_keep_named_ink_tints_and_do_not_invent_rgb_conversion() {
        assert_eq!(tones(&serde_json::json!({"tint":0.25}), Some("Original white")).unwrap()["Original white"], 0.25);
        assert!(tones(&serde_json::json!({"color":{"model":"rgb","r":1,"g":0,"b":0}}), None).is_err());
        assert!(tones(&serde_json::json!({}), Some("Cyan")).is_err());
    }
}
