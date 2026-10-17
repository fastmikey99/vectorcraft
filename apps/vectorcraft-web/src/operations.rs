//! Native-testable operations behind the embedded editor bridge.
use crate::protocol;
use vectorcraft_ui_egui::VectorcraftApp;

/// The active document written as `export` asks → its file name and bytes.
/// → the file's name, its bytes, the selection's bounds (with `selected_only`), and whether the bytes
/// are a selected image's own pixels.
pub fn write_export(app: &mut VectorcraftApp, export: &protocol::Export) -> Result<(String, Vec<u8>, Option<vectorcraft_geom::Rect>, bool), String> {
    use vectorcraft_engine::cmd::fileio;
    let st = app.session.active().ok_or("vectorcraft:export: no document is open")?;
    let title = st.title();
    let stem = std::path::Path::new(&title).file_stem().map_or_else(|| title.clone(), |s| s.to_string_lossy().to_string());
    let selected = st.selection.objects.clone();
    // Only one image's own pixels will do: anything else is refused, saying what is selected.
    if export.image_only {
        let picked: Vec<protocol::Picked> = selected
            .iter()
            .map(|id| match st.doc.node(*id).map(|n| &n.kind) {
                Some(vectorcraft_doc::NodeKind::Image(_)) => protocol::Picked::Image,
                Some(vectorcraft_doc::NodeKind::Group { .. }) => protocol::Picked::Group,
                Some(vectorcraft_doc::NodeKind::Path { .. }) => protocol::Picked::Path,
                Some(vectorcraft_doc::NodeKind::Text(_)) => protocol::Picked::Type,
                _ => protocol::Picked::Other,
            })
            .collect();
        if let Some(why) = protocol::image_only_refusal(&picked) {
            return Err(why);
        }
    }
    if export.selected_only && selected.is_empty() {
        return Err("vectorcraft:export: select something to export (selectedOnly)".to_string());
    }
    // The selection's bounds in document points (what `vectorcraft:place` takes as `at`).
    let bounds =
        export.selected_only.then(|| selected.iter().filter_map(|id| st.doc.node(*id)?.geometric_bounds()).reduce(|a, b| a.union(b))).flatten();
    // One selected image, asked for as a raster: its own pixels as embedded, not drawn again.
    let one_image = match selected.as_slice() {
        [id] if export.selected_only && protocol::is_raster(export.format) => {
            st.doc.node(*id).filter(|n| matches!(n.kind, vectorcraft_doc::NodeKind::Image(_))).map(|_| *id)
        }
        _ => None,
    };
    if let Some(id) = one_image {
        let r = app.session.execute("links.unembed", &serde_json::json!({ "id": id.0 })).map_err(|e| e.to_string())?;
        let name = r.get("name").and_then(serde_json::Value::as_str).unwrap_or("image.png").to_string();
        let data = r.get("dataBase64").and_then(serde_json::Value::as_str).and_then(vectorcraft_format::base64_decode);
        return Ok((name, data.ok_or("vectorcraft:export: the image's pixels couldn't be read")?, bounds, true));
    }
    let mut params = serde_json::json!({ "selectedOnly": export.selected_only });
    if let (Some(keep), Some(o)) = (export.preserve_editing, params.as_object_mut()) {
        o.insert("preserveEditing".into(), keep.into());
    }
    if export.preserve_source {
        params["preset"] = "Separation Roundtrip".into();
        params["output"] = serde_json::json!({ "conversion": "preserveSource" });
    }
    let doc = fileio::export_source(st, &params).map_err(|e| e.to_string())?;
    let (bytes, _warnings) = fileio::encode_with_warnings(&doc, export.format, &params).map_err(|e| e.to_string())?;
    Ok((format!("{stem}.{}", export.extension), bytes, bounds, false))
}

/// Place `place`'s file in place of the one selected object (with nothing selected, at `at`) →
/// the ids of what was placed. One undo step (`file.place`).
pub fn place_file(app: &mut VectorcraftApp, place: &protocol::Place) -> Result<Vec<u64>, String> {
    let st = app.session.active().ok_or("vectorcraft:place: no document is open")?;
    let selected = st.selection.objects.clone();
    let replace = match selected.as_slice() {
        [] => false,
        [_] => true,
        _ => return Err("vectorcraft:place: select the one object to replace".to_string()),
    };
    let mut params = serde_json::json!({
        "name": place.name,
        "dataBase64": vectorcraft_format::base64_encode(&place.bytes),
        "replace": replace,
        // The file's own frame (an SVG's viewBox, a PDF's page) is what fits, not its art's
        // bounds: a traced image whose art stops short of its edges keeps its size and place.
        "crop": "crop",
    });
    let Some(o) = params.as_object_mut() else { return Err("vectorcraft:place: couldn't build the command".to_string()) };
    match (place.at, replace) {
        (Some(at), _) => {
            o.insert("rect".into(), serde_json::json!(at));
        }
        // No rect: the selected object's own frame, through its transform (an image rotated,
        // mirrored or scaled keeps it, and art traced from its pixels lies where they did).
        (None, true) => {
            o.insert("fitReplaced".into(), true.into());
        }
        (None, false) => return Err("vectorcraft:place: select the object to replace, or give `at`".to_string()),
    }
    let r = app.run("file.place", params)?;
    Ok(r.get("ids").and_then(serde_json::Value::as_array).map(|ids| ids.iter().filter_map(serde_json::Value::as_u64).collect()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, ColorMode, Document, Node};
    use vectorcraft_engine::Session;
    use vectorcraft_geom::{Rect, shapes};

    fn app() -> VectorcraftApp {
        let mut doc = Document::new(100.0, 100.0);
        doc.color_mode = ColorMode::Cmyk;
        let layer = doc.layers[0].id;
        let path = Node::path(
            doc.alloc_id(),
            shapes::rectangle(Rect::new(10.0, 10.0, 30.0, 30.0)),
            Appearance::basic(Paint::solid(Color::rgb(0.12345, 0.5, 0.98765)), Paint::None, 0.0),
        );
        doc.insert(Some(layer), 0, path).unwrap();
        let mut session = Session::new();
        session.add_document(doc, None);
        VectorcraftApp::new(session, Default::default())
    }

    fn request() -> protocol::Export {
        protocol::with_output(protocol::export(None, Some(true), None, Some(false), None).unwrap(), Some(r#"{"conversion":"preserveSource"}"#))
            .unwrap()
    }

    #[test]
    fn hybrid_export_keeps_fractional_rgb_in_a_cmyk_document_and_waits_for_delivery_before_marking_saved() {
        let mut app = app();
        app.run("select.all", json!({})).unwrap();
        app.run("object.move", json!({"dx": 1, "dy": 0})).unwrap();
        assert!(app.session.active().unwrap().is_dirty());
        let (name, bytes, bounds, native) = write_export(&mut app, &request()).unwrap();
        assert!(name.ends_with(".pdf") && bounds.is_none() && !native);
        assert!(app.session.active().unwrap().is_dirty(), "encoding is not delivery to the parent");
        assert!(String::from_utf8_lossy(&bytes).contains("/OCProperties"), "Hybrid must retain PDF layers");
        let pdf = craft_pdf::syntax::Pdf::new(Arc::new(bytes)).unwrap();
        let pages = pdf.pages();
        let page = pages.iter().next().unwrap();
        // Layer exports wrap the paint in a Form XObject; inspect the actual operands there.
        let form = page.resources().x_objects.get::<craft_pdf::syntax::object::Stream<'_>>(b"x0").unwrap();
        let decoded = form.decoded().unwrap();
        let content = String::from_utf8_lossy(&decoded);
        assert!(content.contains("0.12345 0.5 0.98765 rg"), "{content}");
    }

    #[test]
    fn unknown_or_non_pdf_output_policy_is_refused() {
        for policy in [r#"{"conversion":"destination"}"#, r#"{"conversion":"preserveSource","destination":"CMYK"}"#, "null", "bad"] {
            assert!(protocol::with_output(request(), Some(policy)).is_err());
        }
        let png = protocol::export(Some("png".into()), None, None, None, None).unwrap();
        assert!(protocol::with_output(png, Some(r#"{"conversion":"preserveSource"}"#)).is_err());
    }

    #[test]
    fn missing_document_and_image_only_refusals_do_not_export_a_page() {
        let mut empty = VectorcraftApp::new(Session::new(), Default::default());
        assert!(write_export(&mut empty, &request()).unwrap_err().contains("no document"));
        let mut app = app();
        app.run("select.all", json!({})).unwrap();
        let image = protocol::export(Some("png".into()), None, None, None, Some(true)).unwrap();
        assert!(write_export(&mut app, &image).unwrap_err().contains("a path is selected"));
    }

    #[test]
    fn placed_vector_replaces_the_selection_and_undo_restores_it() {
        let mut app = app();
        app.run("select.all", json!({})).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        let place = protocol::place(
            Some("trace.svg".into()),
            Some(br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><circle cx="10" cy="10" r="8"/></svg>"#.to_vec()),
            None,
        )
        .unwrap();
        let ids = place_file(&mut app, &place).unwrap();
        assert_eq!(ids.len(), 1);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc, before);
    }
}
