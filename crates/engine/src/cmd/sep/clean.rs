//! Preparation clipping uses the existing curve-preserving boolean and stroke outliner.
use super::super::pathops;
use super::{Element, Result, error, flag};
use std::sync::Arc;
use vectorcraft_color::BlendMode;
use vectorcraft_doc::{Document, Node, NodeKind};
use vectorcraft_geom::{FillRule, PathData};
use vectorcraft_pathops::{BoolOp, DEFAULT_PRECISION, try_boolean};

pub(super) struct Options {
    clip: bool,
    groups: bool,
    text: bool,
    strokes: bool,
}
impl Options {
    pub fn read(p: &Element) -> Result<Self> {
        for key in ["MergeSimilarPathAndText", "OptimizeArtworks", "SplitErraticTextBoxes"] {
            if flag(p, key)? {
                return Err(error(format!("CleanObjects {key}=1 is not implemented")));
            }
        }
        Ok(Self {
            clip: flag(p, "ExecuteClip")?,
            groups: flag(p, "CleanUpGroupsAndClippingMasks")?,
            text: flag(p, "ContourizeText")?,
            strokes: flag(p, "OutlineStrokes")?,
        })
    }
}
fn neutral(n: &Node) -> bool {
    n.opacity == 1.0
        && n.blend == BlendMode::Normal
        && !n.isolate
        && n.extras().mask.is_none()
        && n.appearance.items.is_empty()
        && n.appearance.effects.is_empty()
}
pub(super) fn clean(d: &mut Document, mut n: Node, o: &Options, brushes: &[vectorcraft_brush::Brush]) -> Result<(Option<Node>, usize)> {
    let mut changed = 0;
    if o.text
        && let NodeKind::Text(t) = &n.kind
    {
        let p = pathops::text_outline(t);
        let app = t.first_style().appearance();
        n.kind = Node::path(n.id, p, app.clone()).kind;
        n.appearance = app.into();
        changed += 1;
    }
    if o.strokes
        && let Some(mut outline) = pathops::outline_strokes(d, brushes, &n)
    {
        outline.id = n.id;
        n = outline;
        changed += 1;
    }
    if let Some(children) = n.children_mut() {
        let mut out = Vec::new();
        for c in std::mem::take(children) {
            let (child, count) = clean(d, (*c).clone(), o, brushes)?;
            changed += count;
            if let Some(child) = child {
                out.push(Arc::new(child));
            }
        }
        *children = out;
    }
    if o.clip && n.clips() {
        let children = n.children_mut().ok_or_else(|| error("clip is not a container"))?;
        if children.is_empty() {
            return Ok((None, changed + 1));
        }
        let mask = children.remove(0);
        let (path, rule) = pathops::node_path(&mask).ok_or_else(|| error("clip mask cannot be converted to a path"))?;
        let mut out = Vec::new();
        for child in std::mem::take(children) {
            if let Some(c) = intersect(d, (*child).clone(), &path, rule, brushes)? {
                out.push(Arc::new(c));
            }
        }
        *children = out;
        n.set_clips(false);
        changed += 1;
    }
    if o.groups && matches!(n.kind, NodeKind::Group { clip: false, .. }) {
        if n.children().is_some_and(|c| c.is_empty()) {
            return Ok((None, changed + 1));
        }
        if n.name.is_none()
            && neutral(&n)
            && n.children().is_some_and(|c| c.len() == 1)
            && let Some(child) = n.children().and_then(|c| c.first())
        {
            let mut child = (**child).clone();
            child.id = n.id;
            return Ok((Some(child), changed + 1));
        }
    }
    Ok((Some(n), changed))
}
fn intersect(d: &mut Document, mut n: Node, clip: &PathData, rule: FillRule, brushes: &[vectorcraft_brush::Brush]) -> Result<Option<Node>> {
    // Clipping a stroked path's centreline changes its caps. Outline first so the intersection
    // preserves the painted region, including strokes crossing the clipping boundary.
    if let Some(mut out) = pathops::outline_strokes(d, brushes, &n) {
        out.id = n.id;
        n = out;
    }
    if let Some((path, fill_rule)) = pathops::node_path(&n) {
        let clipped = try_boolean(&path, fill_rule, clip, rule, BoolOp::Intersect, DEFAULT_PRECISION).map_err(error)?;
        if clipped.is_empty() {
            return Ok(None);
        }
        n.kind = Node::path(n.id, clipped, (*n.appearance).clone()).kind;
        if let NodeKind::Path { rule, .. } = &mut n.kind {
            *rule = FillRule::NonZero;
        }
        return Ok(Some(n));
    }
    if let Some(children) = n.children_mut() {
        let mut out = Vec::new();
        for c in std::mem::take(children) {
            if let Some(c) = intersect(d, (*c).clone(), clip, rule, brushes)? {
                out.push(Arc::new(c));
            }
        }
        if out.is_empty() {
            return Ok(None);
        }
        *children = out;
        return Ok(Some(n));
    }
    // Images and meshes remain clipped vector containers: their samples are never resampled.
    // This is the exact painted result of executing a clipping mask for non-path content.
    let mut mask = Node::path(d.alloc_id(), clip.clone(), Default::default());
    if let NodeKind::Path { clipping, rule: r, .. } = &mut mask.kind {
        *clipping = true;
        *r = rule;
    }
    let id = n.id;
    n.id = d.alloc_id();
    let mut group = Node::group(id, vec![Arc::new(mask), Arc::new(n)]);
    group.set_clips(true);
    Ok(Some(group))
}
