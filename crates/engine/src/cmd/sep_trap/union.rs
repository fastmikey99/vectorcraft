//! Paint occupancy from vector outlines, rasterised as one union rather than separate shapes.
use super::*;
use vectorcraft_color::BlendMode;
use vectorcraft_doc::AppearanceItem;
use vectorcraft_geom::FillRule;
use vectorcraft_pathops as po;

fn normalized(path: &PathData, rule: FillRule) -> Result<PathData> {
    po::try_normalize(path, rule).map_err(|e| bad(C, format!("art union: {e}")))
}

fn unite(paths: Vec<PathData>) -> Result<PathData> {
    let paths: Vec<_> = paths.into_iter().filter(|p| !p.is_empty()).collect();
    if paths.is_empty() {
        return Ok(PathData::default());
    }
    // Each input has already passed the fallible normalizer. The bulk union reports a failed
    // final sweep as an empty path; a nonempty union of valid nonempty areas cannot be empty.
    let refs: Vec<_> = paths.iter().map(|p| (p, FillRule::NonZero)).collect();
    let path = po::unite_all(&refs);
    if path.is_empty() {
        return Err(bad(C, "could not unite artwork outlines"));
    }
    Ok(path)
}

/// None keeps the renderer's occupancy for transparency, masks and live effects. Flattening
/// those into opaque outlines would change what is occupied. Ordinary fills/strokes and clips
/// use the same outline geometry as the renderer, including compound fill rules and dashes.
fn painted(n: &Node) -> Result<Option<PathData>> {
    crate::cancel::check()?;
    if !n.visible || n.opacity <= 0.0 || matches!(n.kind, NodeKind::Path { guide: true, .. }) {
        return Ok(Some(PathData::default()));
    }
    if n.opacity != 1.0
        || n.blend != BlendMode::Normal
        || n.knockout != Default::default()
        || !n.appearance.effects.is_empty()
        || has_render_extras(n)
    {
        return Ok(None);
    }
    match &n.kind {
        NodeKind::Path { .. } | NodeKind::Compound { .. } => {
            let (path, rule) = super::super::pathops::node_path(n).ok_or_else(|| bad(C, "missing art outline"))?;
            let mut paths = Vec::new();
            for item in &n.appearance.items {
                match item {
                    AppearanceItem::Fill(f) if f.visible && !f.paint.is_none() && f.opacity > 0.0 => {
                        if f.opacity != 1.0 || f.blend != BlendMode::Normal || !f.effects.is_empty() {
                            return Ok(None);
                        }
                        paths.push(normalized(&path, rule)?);
                    }
                    AppearanceItem::Stroke(s) if s.visible && !s.paint.is_none() && s.width > 0.0 && s.opacity > 0.0 => {
                        if s.opacity != 1.0 || s.blend != BlendMode::Normal || !s.effects.is_empty() || s.brush.is_some() {
                            return Ok(None);
                        }
                        let outline = vectorcraft_render::effects::stroke::outline_region(&path, rule, s);
                        paths.push(normalized(&outline, FillRule::NonZero)?);
                    }
                    _ => {}
                }
            }
            Ok(Some(unite(paths)?))
        }
        NodeKind::Group { children, clip } | NodeKind::Layer { children, clip, .. } => {
            if !n.appearance.items.is_empty() {
                return Ok(None);
            }
            let mut paths = Vec::new();
            for child in children.iter().skip(usize::from(*clip)) {
                let Some(path) = painted(child)? else {
                    return Ok(None);
                };
                paths.push(path);
            }
            let mut path = unite(paths)?;
            if *clip {
                let Some(first) = children.first() else {
                    return Ok(Some(PathData::default()));
                };
                let Some((clip_path, rule)) = super::super::pathops::node_path(first) else {
                    return Ok(None);
                };
                path = po::try_boolean(&path, FillRule::NonZero, &clip_path, rule, po::BoolOp::Intersect, po::DEFAULT_PRECISION)
                    .map_err(|e| bad(C, format!("art union clip: {e}")))?;
                // A clipping path can itself paint, outside the clipped children's area.
                let Some(own) = painted(first)? else {
                    return Ok(None);
                };
                path = unite(vec![path, own])?;
            }
            Ok(Some(path))
        }
        _ => Ok(None),
    }
}

pub(super) fn artwork(doc: &Document) -> Result<Option<PathData>> {
    let mut paths = Vec::new();
    for layer in &doc.layers {
        let Some(path) = painted(layer)? else {
            return Ok(None);
        };
        paths.push(path);
    }
    unite(paths).map(Some)
}
