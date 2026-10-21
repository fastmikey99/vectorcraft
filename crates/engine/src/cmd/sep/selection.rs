use super::{Element, Result, error, flag, required};
use std::collections::BTreeMap;
use vectorcraft_color::Paint;
use vectorcraft_doc::{AppearanceItem, Document, Node, NodeId, NodeKind};

pub(super) fn matches(text: &str, pattern: &str, ignore_case: bool) -> bool {
    let (text, pattern) = if ignore_case { (text.to_lowercase(), pattern.to_lowercase()) } else { (text.into(), pattern.into()) };
    let t: Vec<_> = text.chars().collect();
    let p: Vec<_> = pattern.chars().collect();
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while i < t.len() {
        if p.get(j) == t.get(i) || p.get(j) == Some(&'?') {
            i += 1;
            j += 1;
        } else if p.get(j) == Some(&'*') {
            star = Some(j);
            j += 1;
            mark = i;
        } else if let Some(s) = star {
            mark += 1;
            i = mark;
            j = s + 1;
        } else {
            return false;
        }
    }
    while p.get(j) == Some(&'*') {
        j += 1;
    }
    j == p.len()
}
pub(super) fn layer_ids(doc: &Document) -> Vec<NodeId> {
    let mut out = Vec::new();
    doc.walk(|n| {
        if matches!(n.kind, NodeKind::Layer { .. }) {
            out.push(n.id)
        }
    });
    out
}
pub(super) fn all_objects(doc: &Document, selectable: bool) -> Vec<NodeId> {
    fn walk(n: &Node, selectable: bool, out: &mut Vec<NodeId>) {
        if selectable && (n.locked || !n.visible) {
            return;
        }
        if !matches!(n.kind, NodeKind::Layer { .. }) {
            out.push(n.id);
        }
        if let Some(children) = n.children() {
            for c in children {
                walk(c, selectable, out);
            }
        }
    }
    let mut out = Vec::new();
    for l in &doc.layers {
        walk(l, selectable, &mut out);
    }
    out
}
pub(super) fn merge<T: PartialEq + Clone>(old: &mut Vec<T>, found: Vec<T>, mode: &str) -> Result<()> {
    match mode {
        "NewSelection" => *old = found,
        "AddSelection" | "AddToSelection" => {
            for id in found {
                if !old.contains(&id) {
                    old.push(id);
                }
            }
        }
        "RemoveSelection" | "RemoveFromSelection" => old.retain(|id| !found.contains(id)),
        "IntersectSelection" | "IntersectWithSelection" => old.retain(|id| found.contains(id)),
        _ => return Err(error(format!("unsupported selection mode {mode}"))),
    }
    Ok(())
}
fn paint_inks(doc: &Document, p: &Paint, out: &mut BTreeMap<String, f64>) -> Result<()> {
    let mut add = |name: &str, value: f64| {
        let n = out.entry(name.into()).or_default();
        *n = n.max(value);
    };
    match p {
        Paint::None => {}
        Paint::Solid { swatch: Some(name), tint, .. } if doc.swatch(name).is_some_and(|w| w.spot) => add(name, f64::from(*tint)),
        Paint::Solid { color, .. } => {
            for (name, v) in
                ["Cyan", "Magenta", "Yellow", "Black"].into_iter().zip(super::profiles::cmyk(*color, super::profiles::document_cms(doc).as_ref()))
            {
                add(name, f64::from(v));
            }
        }
        Paint::Gradient(g) => {
            for s in &g.gradient.stops {
                paint_inks(doc, &Paint::Solid { color: s.color, swatch: s.swatch.clone(), tint: s.tint }, out)?;
            }
        }
        Paint::Pattern { .. } => return Err(error("separation selection on pattern paint is not implemented")),
    }
    Ok(())
}
pub(super) fn inks(doc: &Document, n: &Node) -> Result<BTreeMap<String, f64>> {
    let mut out = BTreeMap::new();
    for item in &n.appearance.items {
        match item {
            AppearanceItem::Fill(f) if f.visible => paint_inks(doc, &f.paint, &mut out)?,
            AppearanceItem::Stroke(s) if s.visible && s.width > 0.0 => paint_inks(doc, &s.paint, &mut out)?,
            _ => {}
        }
    }
    if let NodeKind::Mesh(mesh) = &n.kind {
        for point in &mesh.points {
            if point.opacity > 0.0 {
                paint_inks(doc, &Paint::solid(point.color), &mut out)?;
            }
        }
    }
    if let Some(children) = n.children() {
        for child in children.iter().skip(usize::from(n.clips())) {
            for (k, v) in inks(doc, child)? {
                let value = out.entry(k).or_default();
                *value = value.max(v);
            }
        }
    }
    Ok(out)
}
pub(super) fn rule(doc: &Document, ids: &[NodeId], node: Option<&Node>, r: &Element) -> Result<bool> {
    let kind = r.attributes.get("Type").map(String::as_str).ok_or_else(|| error("selection rule has no Type"))?;
    if kind == "ObjectSelectionRule" {
        return match required(r, "ObjectRelation")? {
            "OneOrMoreObjectsSelection" => Ok(!ids.is_empty()),
            "NoObjectsSelection" | "NoObjectSelection" => Ok(ids.is_empty()),
            v => Err(error(format!("unsupported ObjectRelation {v}"))),
        };
    }
    let n = node.ok_or_else(|| error("object rule requires an object"))?;
    match kind {
        "NameRule" => {
            let text = n.name.as_deref().unwrap_or("");
            let pattern = required(r, "SearchString")?;
            let ignore = flag(r, "IgnoreCase")?;
            let (t, p) = if ignore { (text.to_lowercase(), pattern.to_lowercase()) } else { (text.into(), pattern.into()) };
            match required(r, "NameRelation")? {
                "Equals" => Ok(t == p),
                "NotEquals" => Ok(t != p),
                "Contains" => Ok(t.contains(&p)),
                "BeginsWith" => Ok(t.starts_with(&p)),
                "EndsWith" => Ok(t.ends_with(&p)),
                v => Err(error(format!("unsupported NameRelation {v}"))),
            }
        }
        "ObjectTypeRule" => match (required(r, "ObjectType")?, r.value("Option").unwrap_or("")) {
            ("Path", "Clipping") => Ok(matches!(n.kind, NodeKind::Path { clipping: true, .. })),
            ("Path", "" | "PathAny") => Ok(matches!(n.kind, NodeKind::Path { .. } | NodeKind::Compound { .. })),
            ("Image", "ImageAny" | "") => Ok(matches!(n.kind, NodeKind::Image(_))),
            ("Text", _) => Ok(matches!(n.kind, NodeKind::Text(_))),
            ("Group", _) => Ok(matches!(n.kind, NodeKind::Group { .. })),
            (t, o) => Err(error(format!("unsupported ObjectTypeRule {t}/{o}"))),
        },
        "SeparationNameRule" | "SeparationTypeRule" => {
            if r.value("AppearanceScope").unwrap_or("Object") != "Object" {
                return Err(error("unsupported separation AppearanceScope"));
            }
            let epsilon = r
                .value("Epsilon")
                .unwrap_or("0.005")
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| error("invalid separation epsilon"))?;
            let mut values = Vec::new();
            for (name, v) in inks(doc, n)? {
                let matched = if kind == "SeparationNameRule" {
                    matches(&name, required(r, "SeparationName")?, flag(r, "IgnoreCase")?)
                } else {
                    match required(r, "SeparationType")? {
                        "Any" => true,
                        "Spot" => doc.swatch(&name).is_some_and(|s| s.spot),
                        "Process" => !doc.swatch(&name).is_some_and(|s| s.spot),
                        v => return Err(error(format!("unsupported SeparationType {v}"))),
                    }
                };
                if matched {
                    values.push(v);
                }
            }
            match required(r, "SeparationRelation")? {
                "Using" => Ok(values.iter().any(|v| *v > epsilon)),
                "NotUsing" => Ok(values.iter().all(|v| *v <= epsilon)),
                v => Err(error(format!("unsupported SeparationRelation {v}"))),
            }
        }
        _ => Err(error(format!("unsupported selection rule {kind}"))),
    }
}
pub(super) fn condition(doc: &Document, ids: &[NodeId], p: &Element) -> Result<bool> {
    let rules = p.children.iter().filter(|e| e.name == "Rule").map(|r| rule(doc, ids, None, r)).collect::<Result<Vec<_>>>()?;
    match p.value("Operator").unwrap_or("All") {
        "All" => Ok(rules.iter().all(|v| *v)),
        "Any" => Ok(rules.iter().any(|v| *v)),
        "None" => Ok(rules.iter().all(|v| !*v)),
        v => Err(error(format!("unsupported conditional operator {v}"))),
    }
}
