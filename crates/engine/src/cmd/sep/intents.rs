//! Explicit artist selections, evaluated at the preparation/recipe boundary.
use super::{Player, Result, error, selection};
use serde::Deserialize;
use serde_json::{Value, json};
use vectorcraft_doc::{Document, NodeId, NodeKind};
use vectorcraft_geom::Rect;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Intents {
    #[serde(default)]
    metallic: Vec<Intent>,
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

pub(super) fn read(p: &Value) -> Result<Intents> {
    let intents: Intents =
        serde_json::from_value(p.get("intents").cloned().unwrap_or_else(|| json!({}))).map_err(|_| error("invalid metallic intents"))?;
    if intents.metallic.len() > 256 || intents.metallic.iter().any(|i| i.list.is_empty()) {
        return Err(error("invalid metallic intents"));
    }
    Ok(intents)
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
