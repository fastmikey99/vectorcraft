//! Production-facing ink groups supplied by the host's recipe data. No ink-name heuristics.
//! All configuration/visibility is view state: it never renames, hides or removes exported art.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use vectorcraft_color::cms::PROCESS_PLATES;
use vectorcraft_doc::{Document, Node, NodeId, NodeKind};
use vectorcraft_render::proof;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub label: String,
    pub inks: Vec<Ink>,
    pub source_layers: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ink {
    pub name: String,
    pub destination: Destination,
    pub components: Vec<Component>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Destination {
    Indigo,
    Screen,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub name: String,
    pub layer: String,
}
#[derive(Clone, Debug, Default)]
pub struct State {
    pub context: Option<Context>,
    /// Per-document ink visibility. Never changes the global proof view or saved artwork.
    pub plates: Option<Vec<String>>,
    pub hidden: BTreeSet<NodeId>,
    /// Overprint Preview in the production panel: inks see-through (multiplied) when on, each
    /// ink covering what is under it when off (the default; operators turn it on as needed).
    pub overprint: bool,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub name: String,
    pub destination: Option<Destination>,
    pub plates: Vec<String>,
    pub ids: Vec<NodeId>,
    pub rgb: [f32; 3],
    pub components: Vec<(String, Vec<NodeId>)>,
}

impl Context {
    pub fn validate(&self) -> std::result::Result<(), String> {
        let valid = |s: &str| !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control);
        if !valid(&self.label) || self.inks.len() > 4096 || self.source_layers.len() > 200 || self.source_layers.iter().any(|n| !valid(n)) {
            return Err("Invalid separation context or too many entries".into());
        }
        let mut names = BTreeSet::new();
        for i in &self.inks {
            if !valid(&i.name)
                || PROCESS_PLATES.contains(&i.name.as_str())
                || !names.insert(&i.name)
                || i.components.len() > 100
                || i.components.iter().any(|c| !valid(&c.name) || !valid(&c.layer))
            {
                return Err("Invalid or duplicate separation name/component".into());
            }
        }
        Ok(())
    }
}

/// A COW view of component visibility, also used by tools so hidden groups cannot be hit.
pub fn preview_document(st: &crate::DocState) -> std::borrow::Cow<'_, Document> {
    if st.production_separations.hidden.is_empty() {
        return std::borrow::Cow::Borrowed(&st.doc);
    }
    let mut viewed = (*st.doc).clone();
    for id in &st.production_separations.hidden {
        if let Some(n) = viewed.node_mut(*id) {
            n.visible = false;
        }
    }
    std::borrow::Cow::Owned(viewed)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "production.separations.configure", "Configure Production Separations", [], None,
            "{context: {label, inks: [{name, destination: Indigo|Screen, components: [{name, layer}]}], source_layers: [name]}} host-provided exact names; view state only", has_doc, configure),
        cmd!(query "production.separations.list", "List Production Separations", [], None, "{} grouped inks and editable components", has_doc, list),
        cmd!(query "production.separations.preview", "Preview Production Separations", [], None,
            "{plates?: [name], component?: {ink, name}, reset?: bool, overprint?: bool} change preview only; never the saved artwork", has_doc, preview),
        cmd!(query "production.separations.select", "Select Separation Artwork", [], None,
            "{name, component?: name} select existing editable artwork without changing it", has_doc, select),
    ]
}
fn configure(s: &mut Session, p: &Value) -> Result<Value> {
    let c: Context = serde_json::from_value(p.get("context").cloned().unwrap_or(Value::Null))
        .map_err(|e| bad("production.separations.configure", e.to_string()))?;
    c.validate().map_err(|e| bad("production.separations.configure", e))?;
    let d = s.doc_mut()?;
    let plates = proof::plates(&d.doc).into_iter().map(|p| p.name).collect();
    // The host re-supplies context on each operation: keep the operator's overprint choice.
    let overprint = d.production_separations.overprint;
    d.production_separations = State { context: Some(c), plates: Some(plates), overprint, ..Default::default() };
    d.revision += 1;
    list(s, &json!({}))
}

/// Canvas-only proof configuration: a production preview belongs to this document, not
/// every open file (and never an export). Retain the user's configured proof profile.
pub fn active_proof(st: &crate::DocState) -> Option<proof::ProofSetup> {
    match &st.production_separations.plates {
        Some(plates) => Some(proof::ProofSetup { separations: Some(plates.clone()), ..proof::view().setup }),
        None => proof::active_proof(),
    }
}

/// Whether the canvas draws the production preview with overprints knocking out
/// ([`vectorcraft_render::RenderOptions::knockout_separations`]).
pub fn knockout(st: &crate::DocState) -> bool {
    st.production_separations.plates.is_some() && !st.production_separations.overprint
}

/// Own paints only; descendants are walked separately so selecting a plate does not select
/// a mixed-ink parent group. Images/meshes without a named swatch belong to composite art.
fn own_inks(doc: &Document, n: &Node) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut own = n.clone();
    if let Some(children) = own.children_mut() {
        children.clear();
    }
    vectorcraft_doc::inks::visit_node_colors(&mut own, &mut |_, link, _| {
        if let Some(name) = link.as_ref().filter(|name| doc.swatch(name).is_some_and(|s| s.spot)) {
            names.insert(name.clone());
        } else {
            names.insert("CMYK".to_string());
        }
    });
    if matches!(n.kind, NodeKind::Image(_) | NodeKind::Mesh(_)) {
        names.insert("CMYK".to_string());
    }
    names
}

pub fn rows(s: &Session) -> Vec<Row> {
    let Some(st) = s.active() else { return vec![] };
    let doc = &st.doc;
    let context = st.production_separations.context.as_ref();
    let mut sources = BTreeSet::new();
    if let Some(c) = context {
        doc.walk(|n| {
            if n.name.as_ref().is_some_and(|name| c.source_layers.contains(name)) && matches!(n.kind, NodeKind::Layer { .. }) {
                n.walk(&mut |a| sources.extend(own_inks(doc, a)));
            }
        });
    }
    let mut result = vec![Row {
        name: "CMYK".into(),
        destination: Some(Destination::Indigo),
        plates: PROCESS_PLATES.iter().map(|p| p.to_string()).collect(),
        ids: vec![],
        rgb: [0.3; 3],
        components: vec![],
    }];
    for pl in proof::plates(doc).into_iter().filter(|p| p.spot) {
        let ink = context.and_then(|c| c.inks.iter().find(|i| i.name == pl.name));
        let destination = ink.map(|i| i.destination).or_else(|| sources.contains(&pl.name).then_some(Destination::Indigo));
        let mut components: Vec<(String, Vec<NodeId>)> = vec![];
        if let Some(i) = ink {
            for c in &i.components {
                let mut ids = BTreeSet::new();
                doc.walk(|layer| {
                    if layer.name.as_deref() == Some(&c.layer) && matches!(layer.kind, NodeKind::Layer { .. }) {
                        layer.walk(&mut |n| {
                            if n.name.as_deref() == Some(&c.name) && matches!(n.kind, NodeKind::Group { .. }) {
                                let mut has_ink = false;
                                n.walk(&mut |a| {
                                    has_ink |= own_inks(doc, a).contains(&pl.name);
                                });
                                if has_ink {
                                    ids.insert(n.id);
                                }
                            }
                        });
                    }
                });
                if !ids.is_empty() {
                    if let Some((_, existing)) = components.iter_mut().find(|(name, _)| name == &c.name) {
                        existing.extend(ids);
                    } else {
                        components.push((c.name.clone(), ids.into_iter().collect()));
                    }
                }
            }
        }
        result.push(Row { name: pl.name.clone(), destination, plates: vec![pl.name], ids: vec![], rgb: pl.rgb, components });
    }
    doc.walk(|n| {
        if matches!(n.kind, NodeKind::Layer { .. }) {
            return;
        }
        let names = own_inks(doc, n);
        for row in &mut result {
            if names.contains(&row.name) {
                row.ids.push(n.id);
            }
        }
    });
    // Reporter supplies file links in recipe layer order. Keep that display order rather
    // than alphabetising plate names (which puts WP above WHP). Unknown inks remain stable.
    if let Some(c) = context {
        result
            .sort_by_key(|row| if row.name == "CMYK" { 0 } else { c.inks.iter().position(|ink| ink.name == row.name).map_or(usize::MAX, |i| i + 1) });
    }
    result
}
fn list(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(
        json!({"rows": rows(s).iter().map(|r| json!({"name":r.name,"destination":r.destination,"plates":r.plates,"ids":r.ids.iter().map(|i|i.0).collect::<Vec<_>>(),"components":r.components.iter().map(|(n,ids)|json!({"name":n,"ids":ids.iter().map(|i|i.0).collect::<Vec<_>>()})).collect::<Vec<_>>()})).collect::<Vec<_>>()}),
    )
}
fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "production.separations.preview";
    if let Some(on) = p.get("overprint").and_then(Value::as_bool) {
        s.doc_mut()?.production_separations.overprint = on;
    } else if p.get("reset").and_then(Value::as_bool) == Some(true) {
        s.doc_mut()?.production_separations.hidden.clear();
        let plates: Vec<_> = proof::plates(&s.doc()?.doc).into_iter().map(|p| p.name).collect();
        s.doc_mut()?.production_separations.plates = Some(plates);
    } else if let Some(c) = p.get("component") {
        let rs = rows(s);
        let ids = rs
            .iter()
            .find(|r| Some(r.name.as_str()) == c.get("ink").and_then(Value::as_str))
            .and_then(|r| r.components.iter().find(|(name, _)| Some(name.as_str()) == c.get("name").and_then(Value::as_str)))
            .map(|(_, ids)| ids.clone())
            .ok_or_else(|| bad(C, "No such editable component in this file"))?;
        let d = s.doc_mut()?;
        let all_hidden = ids.iter().all(|id| d.production_separations.hidden.contains(id));
        for id in ids {
            if all_hidden {
                d.production_separations.hidden.remove(&id);
            } else {
                d.production_separations.hidden.insert(id);
            }
        }
        d.selection.clear();
    } else if let Some(plates) = p.get("plates") {
        let all: BTreeSet<_> = proof::plates(&s.doc()?.doc).into_iter().map(|p| p.name).collect();
        let plates: Vec<String> = serde_json::from_value(plates.clone()).map_err(|e| bad(C, e.to_string()))?;
        if plates.iter().any(|p| !all.contains(p)) {
            return Err(bad(C, "Unknown separation"));
        }
        s.doc_mut()?.production_separations.plates = Some(plates);
        s.doc_mut()?.selection.clear();
    } else {
        return Err(bad(C, "Choose plates, a component, overprint or reset"));
    }
    s.doc_mut()?.revision += 1;
    ok()
}
fn select(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "production.separations.select";
    let rs = rows(s);
    let row = rs.iter().find(|r| Some(r.name.as_str()) == p.get("name").and_then(Value::as_str)).ok_or_else(|| bad(C, "Unknown separation"))?;
    let ids = if let Some(component) = p.get("component").and_then(Value::as_str) {
        &row.components.iter().find(|(n, _)| n == component).ok_or_else(|| bad(C, "No such editable component in this file"))?.1
    } else {
        &row.ids
    };
    let ids: Vec<_> = ids.iter().map(|id| id.0).collect();
    s.doc_mut()?.production_separations.hidden.clear();
    s.doc_mut()?.revision += 1;
    s.execute("select.set", &json!({"ids":ids}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Session {
        let mut s = Session::new();
        s.execute("file.new", &json!({"width":100,"height":100})).unwrap();
        s.execute("swatch.new", &json!({"name":"WHP","color":"#ff80ff","spot":true})).unwrap();
        s.execute("swatch.new", &json!({"name":"Custom 123","color":"#f0a000","spot":true})).unwrap();
        s.execute("swatch.new", &json!({"name":"Unknown ink","color":"#f08000","spot":true})).unwrap();
        s.execute("layer.new", &json!({"name":"White Underprint-1"})).unwrap();
        for name in ["Visible white", "Underbase"] {
            let id = s.execute("shape.rectangle", &json!({"x":10,"y":10,"width":20,"height":20})).unwrap()["id"].as_u64().unwrap();
            s.execute("paint.setFill", &json!({"ids":[id],"swatch":"WHP"})).unwrap();
            s.execute("paint.setStroke", &json!({"ids":[id],"none":true})).unwrap();
            s.execute("select.set", &json!({"ids":[id]})).unwrap();
            let group = NodeId(s.execute("object.group", &json!({})).unwrap()["id"].as_u64().unwrap());
            std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc).node_mut(group).unwrap().name = Some(name.into());
        }
        s.execute("layer.new", &json!({"name":"Artwork"})).unwrap();
        let id = s.execute("shape.rectangle", &json!({"x":40,"y":40,"width":20,"height":20})).unwrap()["id"].as_u64().unwrap();
        s.execute("paint.setFill", &json!({"ids":[id],"swatch":"Custom 123"})).unwrap();
        s.execute("paint.setStroke", &json!({"ids":[id],"none":true})).unwrap();
        s.doc_mut().unwrap().mark_saved();
        s
    }
    fn configure(s: &mut Session) {
        s.execute(
            "production.separations.configure",
            &json!({"context":{
                "label":"Recipe file links","source_layers":["Artwork"],"inks":[
                    {"name":"WHP","destination":"Screen","components":[
                        {"name":"Visible white","layer":"White Underprint-1"},
                        {"name":"Underbase","layer":"White Underprint-1"}
                    ]}
                ]
            }}),
        )
        .unwrap();
    }
    #[test]
    fn production_separations_group_real_names_and_only_existing_components() {
        let _g = crate::tests_colormgmt::GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = fixture();
        configure(&mut s);
        let rs = rows(&s);
        assert_eq!(rs.iter().filter(|r| r.name == "CMYK").count(), 1);
        assert_eq!(rs.iter().find(|r| r.name == "CMYK").unwrap().plates.len(), 4);
        assert_eq!(rs.iter().find(|r| r.name == "Custom 123").unwrap().destination, Some(Destination::Indigo));
        assert_eq!(rs.iter().find(|r| r.name == "Unknown ink").unwrap().destination, None);
        let white = rs.iter().find(|r| r.name == "WHP").unwrap();
        assert_eq!(white.destination, Some(Destination::Screen));
        assert_eq!(white.components.len(), 2);
        for (_, ids) in &white.components {
            for id in ids {
                std::sync::Arc::make_mut(&mut s.doc_mut().unwrap().doc).node_mut(*id).unwrap().name = None;
            }
        }
        assert!(rows(&s).iter().find(|r| r.name == "WHP").unwrap().components.is_empty(), "legacy combined white isn't split by inference");
        proof::set_view(Default::default());
    }
    #[test]
    fn production_separations_preview_never_changes_saved_art_and_select_edits_only_existing_group() {
        let _g = crate::tests_colormgmt::GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = fixture();
        let before = serde_json::to_value(&s.doc().unwrap().doc).unwrap();
        configure(&mut s);
        s.execute("production.separations.preview", &json!({"plates":["WHP"]})).unwrap();
        s.execute("production.separations.preview", &json!({"component":{"ink":"WHP","name":"Underbase"}})).unwrap();
        assert_eq!(s.doc().unwrap().production_separations.hidden.len(), 1);
        assert!(!s.doc().unwrap().is_dirty());
        assert_eq!(serde_json::to_value(&s.doc().unwrap().doc).unwrap(), before);
        s.execute("production.separations.select", &json!({"name":"WHP","component":"Visible white"})).unwrap();
        assert!(s.doc().unwrap().production_separations.hidden.is_empty());
        assert_eq!(s.doc().unwrap().selection.objects.len(), 1);
        let id = s.doc().unwrap().selection.objects[0];
        assert_eq!(s.doc().unwrap().doc.node(id).unwrap().name.as_deref(), Some("Visible white"));
        s.execute("object.move", &json!({"dx":2,"dy":0})).unwrap();
        assert!(s.doc().unwrap().is_dirty());
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&s.doc().unwrap().doc).unwrap(), before);
        proof::set_view(Default::default());
    }
    #[test]
    fn production_rows_follow_recipe_order_without_ink_name_rules() {
        let _g = crate::tests_colormgmt::GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = fixture();
        s.execute(
            "production.separations.configure",
            &json!({"context": {
                "label":"Recipe order", "source_layers":[], "inks":[
                    {"name":"Unknown ink","destination":"Screen","components":[]},
                    {"name":"Custom 123","destination":"Screen","components":[]},
                    {"name":"WHP","destination":"Screen","components":[]}
                ]
            }}),
        )
        .unwrap();
        let names: Vec<_> = rows(&s).into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["CMYK", "Unknown ink", "Custom 123", "WHP"]);
        proof::set_view(Default::default());
    }

    #[test]
    fn production_preview_load_and_show_all_keep_the_same_composite_without_editing_art() {
        let _g = crate::tests_colormgmt::GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        proof::set_view(Default::default());
        let mut s = fixture();
        // An opaque plate drawn over artwork reproduces the production-PDF display problem:
        // ordinary viewing covers the artwork, separation preview honours the overprint.
        s.execute("swatch.new", &json!({"name":"WP","color":"#d4dc98","spot":true})).unwrap();
        let id = s.execute("shape.rectangle", &json!({"x":0,"y":0,"width":100,"height":100})).unwrap()["id"].as_u64().unwrap();
        s.execute("paint.setFill", &json!({"ids":[id],"swatch":"WP"})).unwrap();
        s.execute("paint.setStroke", &json!({"ids":[id],"none":true})).unwrap();
        let d = s.doc_mut().unwrap();
        let n = std::sync::Arc::make_mut(&mut d.doc).node_mut(NodeId(id)).unwrap();
        for item in &mut n.appearance.items {
            *item.overprint_mut() = true;
        }
        d.mark_saved();
        let before = serde_json::to_value(&s.doc().unwrap().doc).unwrap();
        let mut renderer = vectorcraft_render::Renderer::new();
        renderer.threads = 0;
        let render = |renderer: &mut vectorcraft_render::Renderer, s: &Session| {
            renderer
                .render(
                    &s.doc().unwrap().doc,
                    100,
                    100,
                    vectorcraft_geom::Affine::IDENTITY,
                    &vectorcraft_render::RenderOptions {
                        proof: active_proof(s.doc().unwrap()),
                        overprint_preview: proof::overprint_preview_on(),
                        knockout_separations: knockout(s.doc().unwrap()),
                        ..Default::default()
                    },
                )
                .pixel(50, 50)
        };
        let ordinary = render(&mut renderer, &s);
        configure(&mut s);
        // Overprint Preview starts off: the top powder plate covers the artwork, as ordinary
        // viewing shows it. Turning it on makes the plate see-through.
        assert!(!s.doc().unwrap().production_separations.overprint);
        let covered = render(&mut renderer, &s);
        s.execute("production.separations.preview", &json!({"overprint":true})).unwrap();
        let initial = render(&mut renderer, &s);
        assert_ne!(initial, covered, "overprint preview must make the covering powder plate see-through");
        assert_ne!(initial, ordinary, "overprint preview must not present the covering powder plate");
        // The host re-supplies context on each operation; the operator's choice stays.
        configure(&mut s);
        assert!(s.doc().unwrap().production_separations.overprint);
        s.execute("production.separations.preview", &json!({"overprint":false})).unwrap();
        assert_eq!(render(&mut renderer, &s), covered);
        s.execute("production.separations.preview", &json!({"overprint":true})).unwrap();
        let all = s.doc().unwrap().production_separations.plates.clone().unwrap();
        s.execute("production.separations.preview", &json!({"plates":["WHP"]})).unwrap();
        assert_ne!(render(&mut renderer, &s), initial);
        s.execute("production.separations.preview", &json!({"reset":true})).unwrap();
        assert_eq!(s.doc().unwrap().production_separations.plates, Some(all.clone()));
        assert!(proof::view().separations.is_none(), "production preview must not leak into other documents");
        assert_eq!(render(&mut renderer, &s), initial);
        // Returning each eye to visible produces the same image as Show all.
        s.execute("production.separations.preview", &json!({"plates":all})).unwrap();
        assert_eq!(render(&mut renderer, &s), initial);
        assert_eq!(serde_json::to_value(&s.doc().unwrap().doc).unwrap(), before);
        assert!(!s.doc().unwrap().is_dirty());
        proof::set_view(Default::default());
    }

    #[test]
    fn production_separations_rejects_invalid_context_without_replacing_existing_context() {
        let _g = crate::tests_colormgmt::GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = fixture();
        configure(&mut s);
        let before = s.doc().unwrap().production_separations.context.clone();
        assert!(s.execute("production.separations.configure", &json!({"context":{"label":"","inks":[],"source_layers":[]}})).is_err());
        assert_eq!(s.doc().unwrap().production_separations.context, before);
        proof::set_view(Default::default());
    }
}
