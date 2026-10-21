//! Explicit artist selections; separation rules remain in the supplied PALs.
use super::*;
use serde::Deserialize;
use vectorcraft_doc::{Node, NodeKind};
use vectorcraft_geom::{FillRule, PathData, shapes};
use vectorcraft_pathops::{BoolOp, DEFAULT_PRECISION, try_boolean};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NoTrap {
    #[serde(default, rename = "objectIds")]
    object_ids: Vec<NodeId>,
    #[serde(default)]
    rectangles: Vec<Region>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Region {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    list: String,
    objects: Option<Selector>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Selector {
    Ids(Ids),
    Spot(Spot),
    Rect(Rectangle),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ids {
    ids: Vec<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spot {
    spot: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rectangle {
    #[serde(rename = "rectMm")]
    rect: [f64; 4],
}

#[derive(Default)]
pub(super) struct Intents {
    pub(super) no_trap: Option<NoTrap>,
    metallic: Vec<Intent>,
}

pub(super) fn read(p: &Value) -> Result<Intents> {
    let Some(value) = p.get("intents") else { return Ok(Intents::default()) };
    let values = value.as_object().ok_or_else(|| error("intents must be an object"))?;
    for key in values.keys() {
        if !matches!(key.as_str(), "noTrap" | "metallic") {
            return Err(error(format!("unsupported intent: {key}")));
        }
    }
    // Parse present noTrap values explicitly: null is not an empty selection.
    let no_trap: Option<NoTrap> = values.get("noTrap").map(|v| serde_json::from_value(v.clone()).map_err(error)).transpose()?;
    if let Some(intent) = &no_trap {
        if intent.object_ids.len() > 100_000 || intent.rectangles.len() > 256 {
            return Err(error("noTrap supports at most 100000 object ids and 256 rectangles"));
        }
        for r in &intent.rectangles {
            if [r.x, r.y, r.width, r.height].iter().any(|v| !v.is_finite() || v.abs() > 100_000.0) || r.width <= 0.0 || r.height <= 0.0 {
                return Err(error("noTrap rectangles require finite mm coordinates and positive sizes, bounded to 100000 mm"));
            }
        }
    }
    let metallic: Vec<Intent> = values
        .get("metallic")
        .map(|v| serde_json::from_value(v.clone()).map_err(|_| error("invalid metallic intents")))
        .transpose()?
        .unwrap_or_default();
    if metallic.len() > 256 || metallic.iter().any(|i| i.list.is_empty()) {
        return Err(error("invalid metallic intents"));
    }
    Ok(Intents { no_trap, metallic })
}

fn select(d: &Document, selector: &Selector) -> Result<Vec<NodeId>> {
    let all = selection::all_objects(d, true);
    let ids = match selector {
        Selector::Ids(v) => {
            if v.ids.len() > 100_000 || v.ids.iter().any(|id| !all.contains(&NodeId(*id))) {
                return Err(error("metallic intent contains an unavailable object id"));
            }
            all.into_iter().filter(|id| v.ids.contains(&id.0)).collect()
        }
        Selector::Spot(v) => {
            let mut found = Vec::new();
            // Select atomic objects, never a mixed group just because one descendant uses the ink.
            for id in all {
                if let Some(n) = d.node(id)
                    && (n.children().is_none() || matches!(n.kind, NodeKind::Compound { .. }))
                    && selection::inks(d, n)?.get(&v.spot).is_some_and(|v| *v > 0.005)
                {
                    found.push(id);
                }
            }
            found
        }
        Selector::Rect(v) => {
            let [x, y, w, h] = v.rect;
            if v.rect.iter().any(|v| !v.is_finite() || v.abs() > 1e7) || w <= 0.0 || h <= 0.0 {
                return Err(error("invalid metallic intent rectangle"));
            }
            let mm = 72.0 / 25.4;
            let r = Rect::new(x * mm, y * mm, (x + w) * mm, (y + h) * mm);
            all.into_iter()
                .filter(|id| {
                    d.node(*id).is_some_and(|n| n.children().is_none() || matches!(n.kind, NodeKind::Compound { .. }))
                        && d.bounds_of(&[*id], true).is_some_and(|b| b.x0 >= r.x0 && b.y0 >= r.y0 && b.x1 <= r.x1 && b.y1 <= r.y1)
                })
                .collect()
        }
    };
    Ok(ids)
}
impl Player<'_> {
    pub(super) fn apply_intents(&mut self, intents: &Intents) -> Result<()> {
        let previous = self.ids()?;
        for intent in &intents.metallic {
            let ids = match &intent.objects {
                Some(selector) => select(self.doc()?, selector)?,
                None => Vec::new(),
            };
            if intent.objects.is_some() && ids.is_empty() {
                return Err(error("metallic intent selected no objects"));
            }
            self.trace.push(
                json!({"list":"MetallicIntents","action":"SelectIntent","objects":ids.iter().map(|id|id.0).collect::<Vec<_>>(),"target":intent.list}),
            );
            let d = self.doc()?;
            self.intent_scope = intent.objects.as_ref().map(|_| {
                selection::all_objects(d, true)
                    .into_iter()
                    .filter(|id| ids.contains(id) || d.ancestry(*id).is_some_and(|a| a.iter().any(|p| ids.contains(p))))
                    .collect()
            });
            self.session.doc_mut()?.selection.set(ids);
            let result = self.list(&intent.list, None);
            self.intent_scope = None;
            result?;
            if self.stopped.is_some() {
                break;
            }
        }
        let previous = previous.into_iter().filter(|id| self.doc().is_ok_and(|d| d.node(*id).is_some())).collect::<Vec<_>>();
        self.session.doc_mut()?.selection.set(previous);
        Ok(())
    }
}

impl Player<'_> {
    pub(super) fn apply_no_trap(&mut self, intent: NoTrap) -> Result<()> {
        let doc = self.doc()?;
        let art = selection::layer_ids(doc)
            .into_iter()
            .filter(|id| doc.node(*id).is_some_and(|n| n.name.as_deref() == Some("Artwork")))
            .collect::<Vec<_>>();
        let [art] = art.as_slice() else { return Err(error("noTrap requires exactly one Artwork layer")) };
        let leaves = actions::leaf_targets(doc, &[*art]);
        for id in &intent.object_ids {
            if doc.node(*id).is_none() || *id == *art || !doc.ancestry(*id).is_some_and(|parents| parents.contains(art)) {
                return Err(error(format!("noTrap object {} is not prepared Artwork", id.0)));
            }
        }
        let mut selected = actions::leaf_targets(doc, &intent.object_ids);
        let mut region = PathData::default();
        for r in intent.rectangles {
            let mm = 72.0 / 25.4;
            let rect = shapes::rectangle(Rect::new(r.x * mm, r.y * mm, (r.x + r.width) * mm, (r.y + r.height) * mm));
            region = try_boolean(&region, FillRule::NonZero, &rect, FillRule::NonZero, BoolOp::Union, DEFAULT_PRECISION).map_err(error)?;
        }
        self.session.edit("Select", |d, sel| {
            if !region.is_empty() {
                for id in leaves {
                    vectorcraft_doc::cancel::check().map_err(error)?;
                    if selected.contains(&id) {
                        continue;
                    }
                    let node = d.node(id).cloned().ok_or_else(|| error("noTrap object disappeared"))?;
                    let Some(bounds) = node.visual_bounds() else { continue };
                    if region.bounds().is_none_or(|r| r.intersect(bounds).area() <= 0.0) {
                        continue;
                    }
                    // Prepared flat filled paths can be split without changing their appearance.
                    // Refuse strokes, masks and inherited clipping instead of replacing painted art with bounds.
                    if node.opacity != 1.0
                        || node.blend != vectorcraft_color::BlendMode::Normal
                        || node.extras().mask.is_some()
                        || !node.appearance.effects.is_empty()
                        || node.appearance.items.iter().any(|a| match a {
                            vectorcraft_doc::AppearanceItem::Stroke(s) => !s.paint.is_none() && s.width > 0.0,
                            vectorcraft_doc::AppearanceItem::Fill(f) => {
                                f.opacity != 1.0 || f.blend != vectorcraft_color::BlendMode::Normal || !f.effects.is_empty()
                            }
                        })
                        || d.ancestry(id).is_some_and(|ids| {
                            ids.iter().any(|id| {
                                d.node(*id).is_some_and(|n| {
                                    n.clips()
                                        || n.opacity != 1.0
                                        || n.blend != vectorcraft_color::BlendMode::Normal
                                        || n.extras().mask.is_some()
                                        || !n.appearance.effects.is_empty()
                                })
                            })
                        })
                    {
                        return Err(error("noTrap rectangles require prepared opaque filled paths without strokes, masks, effects or clips"));
                    }
                    let (path, rule) = super::super::pathops::node_path(&node).ok_or_else(|| error("noTrap rectangles require prepared paths"))?;
                    let inside = try_boolean(&path, rule, &region, FillRule::NonZero, BoolOp::Intersect, DEFAULT_PRECISION).map_err(error)?;
                    if inside.is_empty() {
                        continue;
                    }
                    let outside = try_boolean(&path, rule, &region, FillRule::NonZero, BoolOp::Difference, DEFAULT_PRECISION).map_err(error)?;
                    if outside.is_empty() {
                        selected.push(id);
                        continue;
                    }
                    let (parent, at, _) = d.position(id).ok_or_else(|| error("noTrap object position disappeared"))?;
                    let mut piece = node.clone();
                    piece.id = d.alloc_id();
                    piece.kind = Node::path(piece.id, inside, (*piece.appearance).clone()).kind;
                    if let Some(original) = d.node_mut(id) {
                        original.kind = Node::path(id, outside, (*original.appearance).clone()).kind;
                        if let NodeKind::Path { rule, .. } = &mut original.kind {
                            *rule = FillRule::NonZero;
                        }
                    }
                    selected.push(piece.id);
                    d.insert(parent, at + 1, piece)?;
                }
            }
            sel.set(selected.iter().copied());
            Ok(())
        })?;
        self.trace.push(json!({"list":"Intent","position":0,"action":"SelectNoTrap","objectsAffected":selected.len()}));
        writeln!(std::io::stderr(), "[Intent #0] SelectNoTrap → {} objects", selected.len()).map_err(error)
    }
}
