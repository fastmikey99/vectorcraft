//! Shared raw-colour-space tonal reconciliation for native and WebAssembly.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn tones(paint: &Value, ink: Option<&str>) -> Result<BTreeMap<String, f64>, String> {
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
pub(crate) fn source_tones(paint: &crate::pdf::PaintedObject, source: &vectorcraft_pdf::PrintPaintSource) -> Result<BTreeMap<String, f64>, String> {
    use vectorcraft_pdf::PrintColorSpace;
    let mut values = tones(&paint.paint, paint.ink.as_deref())?;
    let model = paint.paint.get("color").and_then(|c| c.get("model")).and_then(Value::as_str);
    match source.color_space {
        PrintColorSpace::DeviceCmyk if paint.ink.is_none() && model == Some("cmyk") => Ok(values),
        PrintColorSpace::DeviceGray if paint.ink.is_none() && model == Some("gray") => Ok(values),
        PrintColorSpace::Separation | PrintColorSpace::DeviceN => {
            let names: BTreeSet<_> = source.colorants.iter().collect();
            if names.is_empty()
                || names.len() != source.colorants.len()
                || (source.color_space == PrintColorSpace::Separation && names.len() != 1)
                || source.colorants.iter().any(|n| ["All", "None"].contains(&n.as_str()))
            {
                return Err("Special, duplicate or unresolved named colourants require explicit PDF reconciliation".into());
            }
            if let Some(ink) = &paint.ink {
                if source.colorants.len() != 1 || source.colorants.first() != Some(ink) {
                    return Err("Decoded spot paint does not retain every original named colourant".into());
                }
            } else if model != Some("cmyk") || source.colorants.iter().any(|n| !["Cyan", "Magenta", "Yellow", "Black"].contains(&n.as_str())) {
                return Err("Named colourants require retained component values".into());
            }
            // A process-named Separation/DeviceN paint writes only its named
            // channels. OPM 1 does not suppress even a zero named component.
            values.retain(|name, _| names.contains(name));
            if values.len() != names.len() {
                return Err("Named colourant components are incomplete".into());
            }
            Ok(values)
        }
        _ => Err("Original colour space/profile needs a qualified tonal prediction".into()),
    }
}

/// Result of one opaque paint on one physical ink. None means unchanged.
/// OPM 1 suppresses zero only for direct DeviceCMYK, never a named tint.
pub(crate) fn overpaint(tones: &BTreeMap<String, f64>, ink: &str, overprint: bool, skip_zero_process: bool) -> Option<f64> {
    if !overprint {
        return Some(tones.get(ink).copied().unwrap_or(0.0));
    }
    tones.get(ink).copied().filter(|tone| !(skip_zero_process && *tone == 0.0))
}

/// OPM zero suppression applies only to original direct DeviceCMYK operands.
pub(crate) fn skip_zero_process(paint: &crate::pdf::PaintedObject, source: &vectorcraft_pdf::PrintPaintSource) -> bool {
    source.color_space == vectorcraft_pdf::PrintColorSpace::DeviceCmyk && !paint.overprint_zero
}
