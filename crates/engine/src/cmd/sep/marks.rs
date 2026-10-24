//! Persistent artist intent; changing it never changes the printed artwork.
use super::*;
use crate::cmd::{has_doc, targets};
use vectorcraft_doc::SepMark;

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "sep.mark.set",
            "Set Separation Mark",
            [],
            None,
            "{ids?: [object ids], mark: {type: metallic, list: relative PAL name} | {type: noTrap}} mark ids or the selection; one undo step → {changed}",
            has_doc,
            set
        ),
        cmd!(
            "sep.mark.clear",
            "Clear Separation Mark",
            [],
            None,
            "{ids?: [object ids]} clear marks on ids or the selection and their descendants; one undo step → {changed}",
            has_doc,
            clear
        ),
        cmd!(
            "sep.mark.list",
            "Separation Marks",
            [],
            None,
            "{} → {marks: [{id, mark, bounds: {x,y,width,height}|null}], units: pt}; bounds use document points, x right and y down",
            has_doc,
            list
        ),
    ]
}
fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let mark: SepMark = serde_json::from_value(p.get("mark").cloned().unwrap_or(Value::Null)).map_err(error)?;
    if !mark.valid() {
        return Err(error("invalid separation mark list"));
    }
    change(s, p, Some(mark))
}
fn clear(s: &mut Session, p: &Value) -> Result<Value> {
    change(s, p, None)
}
fn change(s: &mut Session, p: &Value, mark: Option<SepMark>) -> Result<Value> {
    if p.get("ids").is_some_and(|v| !v.is_array()) {
        return Err(error("ids must be an array"));
    }
    let ids = targets(s, p)?;
    if ids.len() > 100_000 {
        return Err(error("too many separation mark targets"));
    }
    let d = &s.doc()?.doc;
    if ids.iter().any(|id| d.node(*id).is_none()) {
        return Err(error("separation mark target is missing"));
    }
    let mut changes: BTreeMap<NodeId, Option<SepMark>> = BTreeMap::new();
    for id in actions::roots(d, &ids) {
        // Split an inherited group mark onto its children before overriding/clearing one
        // descendant. Siblings retain their intent; clearing the descendant really clears it.
        for parent in d.ancestry(id).unwrap_or_default().into_iter().take_while(|p| *p != id) {
            let Some(n) = d.node(parent) else { continue };
            let inherited = changes.get(&parent).unwrap_or(&n.extras().sep_mark).clone();
            if let Some(inherited) = inherited {
                changes.insert(parent, None);
                for child in n.children().into_iter().flatten() {
                    changes.entry(child.id).or_insert_with(|| child.extras().sep_mark.clone().or(Some(inherited.clone())));
                }
            }
        }
        if let Some(n) = d.node(id) {
            n.walk(&mut |child| {
                changes.insert(child.id, if child.id == id { mark.clone() } else { None });
            });
        }
    }
    changes.retain(|id, mark| s.doc().is_ok_and(|st| st.doc.node(*id).is_some_and(|n| &n.extras().sep_mark != mark)));
    let changed = changes.len();
    if changed != 0 {
        s.edit(if mark.is_some() { "Set Separation Mark" } else { "Clear Separation Mark" }, |d, _| {
            for (id, value) in changes {
                if let Some(n) = d.node_mut(id) {
                    n.extras_mut().sep_mark = value;
                }
            }
            Ok(())
        })?;
    }
    Ok(json!({"changed": changed}))
}
fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let d = &s.doc()?.doc;
    let mut marks = Vec::new();
    d.walk(|n| {
        if let Some(mark) = &n.extras().sep_mark {
            let bounds = d.bounds_of(&[n.id], true).map(|r| json!({"x":r.x0,"y":r.y0,"width":r.width(),"height":r.height()}));
            marks.push(json!({"id":n.id,"mark":mark,"bounds":bounds}));
        }
    });
    Ok(json!({"marks":marks,"units":"pt"}))
}

/// Materialize inherited intent before preparation removes/ungroups containers.
pub(super) fn inherit(d: &mut Document) {
    fn visit(n: &mut vectorcraft_doc::Node, inherited: Option<&SepMark>) {
        let mark = n.extras().sep_mark.as_ref().or(inherited).cloned();
        n.extras_mut().sep_mark = mark.clone();
        if let Some(children) = n.children_mut() {
            for c in children {
                visit(std::sync::Arc::make_mut(c), mark.as_ref());
            }
        }
    }
    for n in &mut d.layers {
        visit(std::sync::Arc::make_mut(n), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clearing_one_child_preserves_its_siblings_group_intent() {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        let a = s.execute("shape.rectangle", &json!({"x":0,"y":0,"width":10,"height":10})).unwrap()["id"].clone();
        let b = s.execute("shape.rectangle", &json!({"x":20,"y":0,"width":10,"height":10})).unwrap()["id"].clone();
        s.execute("select.set", &json!({"ids":[a,b]})).unwrap();
        s.execute("object.group", &json!({})).unwrap();
        s.execute("sep.mark.set", &json!({"mark":{"type":"noTrap"}})).unwrap();
        let before = s.doc().unwrap().doc.clone();
        s.execute("sep.mark.clear", &json!({"ids":[a]})).unwrap();
        let listed = s.execute("sep.mark.list", &json!({})).unwrap();
        assert_eq!(listed["marks"].as_array().unwrap().len(), 1);
        assert_eq!(listed["marks"][0]["id"], b);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.doc().unwrap().doc, before);
    }

    #[test]
    fn selection_marks_roundtrip_native_and_undo_atomically() {
        let mut s = Session::new();
        s.execute("file.new", &json!({})).unwrap();
        s.execute("shape.rectangle", &json!({"x":10,"y":20,"width":30,"height":40})).unwrap();
        let mark = json!({"type":"metallic","list":"(2a) Apply Metallic Colour/Set M-2 objects (NZ_US)"});
        s.execute("sep.mark.set", &json!({"mark":mark})).unwrap();
        let listed = s.execute("sep.mark.list", &json!({})).unwrap();
        assert_eq!(listed["marks"][0]["mark"], mark);
        assert_eq!(listed["units"], "pt");
        let bytes = vectorcraft_format::save(&s.doc().unwrap().doc, false);
        let restored = vectorcraft_format::load(&bytes).unwrap();
        assert_eq!(
            restored.node(s.doc().unwrap().selection.objects[0]).unwrap().extras().sep_mark,
            Some(SepMark::Metallic { list: mark["list"].as_str().unwrap().into() })
        );
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.execute("sep.mark.list", &json!({})).unwrap()["marks"], json!([]));
        s.execute("edit.redo", &json!({})).unwrap();
        s.execute("sep.mark.clear", &json!({})).unwrap();
        assert_eq!(s.execute("sep.mark.list", &json!({})).unwrap()["marks"], json!([]));
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.execute("sep.mark.list", &json!({})).unwrap(), listed);
        for bad in [
            json!({"ids":null,"mark":mark}),
            json!({"mark":{"type":"metallic","list":"../escape"}}),
            json!({"ids":[999999],"mark":{"type":"noTrap"}}),
            json!({"mark":{"type":"noTrap","extra":true}}),
        ] {
            assert!(s.execute("sep.mark.set", &bad).is_err(), "{bad}");
            assert_eq!(s.execute("sep.mark.list", &json!({})).unwrap(), listed);
        }
    }
}
