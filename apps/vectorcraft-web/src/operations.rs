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

/// An image's printed frame: its pixel width and height through its transform, in points.
fn frame(im: &vectorcraft_doc::ImageObject) -> (f64, f64) {
    let [a, b, c, d, _, _] = im.xf.as_coeffs();
    (a.hypot(b) * f64::from(im.width), c.hypot(d) * f64::from(im.height))
}

/// The images in `doc` showing the same pixels as image `id` → (those at its printed size, itself
/// included, in document order; how many at other sizes). Sizes within a hundredth of a point match.
pub fn copies_of(doc: &vectorcraft_doc::Document, id: vectorcraft_doc::NodeId) -> (Vec<vectorcraft_doc::NodeId>, usize) {
    let Some(vectorcraft_doc::NodeKind::Image(me)) = doc.node(id).map(|n| &n.kind) else { return (vec![], 0) };
    let (w, h) = frame(me);
    let (mut same, mut other) = (vec![], 0);
    doc.walk(|n| {
        if let vectorcraft_doc::NodeKind::Image(im) = &n.kind
            && im.key == me.key
            && (im.width, im.height) == (me.width, me.height)
        {
            let (iw, ih) = frame(im);
            if (iw - w).abs() < 0.01 && (ih - h).abs() < 0.01 { same.push(n.id) } else { other += 1 }
        }
    });
    (same, other)
}

/// What is selected in the active document, for `vectorcraft:selection`.
pub fn selection(app: &VectorcraftApp) -> protocol::Selection {
    let Some(st) = app.session.active() else { return protocol::Selection::default() };
    match st.selection.objects.as_slice() {
        [id] if matches!(st.doc.node(*id).map(|n| &n.kind), Some(vectorcraft_doc::NodeKind::Image(_))) => {
            let (same, other_sizes) = copies_of(&st.doc, *id);
            protocol::Selection { image: true, copies: same.len().saturating_sub(1), other_sizes }
        }
        _ => protocol::Selection::default(),
    }
}

/// Place `place`'s file in place of the one selected object (with nothing selected, at `at`) →
/// the ids of what was placed. One undo step (`file.place`; with `copies`, `command.batch`).
pub fn place_file(app: &mut VectorcraftApp, place: &protocol::Place) -> Result<Vec<u64>, String> {
    let st = app.session.active().ok_or("vectorcraft:place: no document is open")?;
    let selected = st.selection.objects.clone();
    if place.copies {
        let [id] = selected.as_slice() else { return Err("vectorcraft:place: select the one image whose copies to replace".to_string()) };
        if !matches!(st.doc.node(*id).map(|n| &n.kind), Some(vectorcraft_doc::NodeKind::Image(_))) {
            return Err("vectorcraft:place: `copies` replaces an image: select one image".to_string());
        }
        let (ids, _) = copies_of(&st.doc, *id);
        let data = vectorcraft_format::base64_encode(&place.bytes);
        let commands: Vec<serde_json::Value> = ids
            .iter()
            .flat_map(|id| {
                [
                    serde_json::json!({ "command": "select.set", "params": { "ids": [id.0] } }),
                    serde_json::json!({ "command": "file.place", "params": { "name": place.name, "dataBase64": data, "replace": true, "crop": "crop", "fitReplaced": true } }),
                ]
            })
            .collect();
        let r = app.run("command.batch", serde_json::json!({ "label": "Place", "commands": commands }))?;
        let placed: Vec<u64> = r
            .get("results")
            .and_then(serde_json::Value::as_array)
            // Every other result is a place's (the select.set steps between answer with ids too).
            .map(|results| {
                results.iter().skip(1).step_by(2).filter_map(|r| r.get("ids")?.as_array()).flatten().filter_map(serde_json::Value::as_u64).collect()
            })
            .unwrap_or_default();
        app.run("select.set", serde_json::json!({ "ids": placed }))?;
        return Ok(placed);
    }
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

/// What a `vectorcraft:solidify` did (`vectorcraft:solidified`): the images changed (none when no
/// pixel was soft), where their transparency is stored (`alpha` or `mask`), the level, and the
/// pixels counted (soft before; clear and opaque after).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Solidified {
    pub ids: Vec<u64>,
    pub form: String,
    pub level: u64,
    pub soft: u64,
    pub clear: u64,
    pub opaque: u64,
}

/// Remove the soft pixels of the one selected image (`object.image.solidifyAlpha`): one undo step,
/// with `copies` also for its copies at the same printed size.
pub fn solidify(app: &mut VectorcraftApp, request: &protocol::Solidify) -> Result<Solidified, String> {
    let st = app.session.active().ok_or("vectorcraft:solidify: no document is open")?;
    let id = match st.selection.objects.as_slice() {
        [id] if matches!(st.doc.node(*id).map(|n| &n.kind), Some(vectorcraft_doc::NodeKind::Image(_))) => *id,
        _ => return Err("vectorcraft:solidify: select one image".to_string()),
    };
    let r = app.run("object.image.solidifyAlpha", serde_json::json!({ "id": id.0, "level": request.level, "copies": request.copies }))?;
    let n = |k: &str| r.get(k).and_then(serde_json::Value::as_u64).ok_or_else(|| format!("vectorcraft:solidify: the answer has no `{k}`"));
    Ok(Solidified {
        ids: r.get("ids").and_then(serde_json::Value::as_array).map(|a| a.iter().filter_map(serde_json::Value::as_u64).collect()).unwrap_or_default(),
        form: r.get("form").and_then(serde_json::Value::as_str).unwrap_or("alpha").to_string(),
        level: n("level")?,
        soft: n("softPixels")?,
        clear: n("clearPixels")?,
        opaque: n("opaquePixels")?,
    })
}

/// Summarize what was actually saved (also used for File > Save downloads and selection exports).
pub fn saved_marks(name: &str, bytes: &[u8]) -> Result<serde_json::Value, String> {
    if !name.to_ascii_lowercase().ends_with(".pdf") {
        return Ok(serde_json::json!([]));
    }
    let loaded = vectorcraft_engine::cmd::fileio::load(name, bytes).map_err(|e| e.to_string())?;
    let mut session = vectorcraft_engine::Session::new();
    session.add_document(loaded.doc, None);
    Ok(session.execute("sep.mark.list", &serde_json::json!({})).map_err(|e| e.to_string())?["marks"].clone())
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

    #[test]
    fn host_marks_export_summary_and_production_reopen() {
        let mut app = app();
        app.run("select.all", json!({})).unwrap();
        let (cmd, params) = protocol::mark_command("vectorcraft:sep.mark.set", Some(json!({"type":"noTrap"}))).unwrap();
        app.run(cmd, params).unwrap();
        let (name, bytes, _, _) = write_export(&mut app, &request()).unwrap();
        let summary = saved_marks(&name, &bytes).unwrap();
        assert_eq!(summary.as_array().unwrap().len(), 1);
        assert_eq!(summary[0]["mark"], json!({"type":"noTrap"}));
        assert!(summary[0]["bounds"]["width"].as_f64().unwrap() > 0.);
        app.run("document.open", json!({"name":"reopen.pdf","dataBase64":vectorcraft_format::base64_encode(&bytes),"printProduction":true})).unwrap();
        assert_eq!(app.run("sep.mark.list", json!({})).unwrap()["marks"].as_array().unwrap().len(), 1);
        let (cmd, params) = protocol::mark_command("vectorcraft:sep.mark.clear", None).unwrap();
        app.run("select.all", json!({})).unwrap();
        app.run(cmd, params).unwrap();
        assert_eq!(app.run("sep.mark.list", json!({})).unwrap()["marks"], json!([]));
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.run("sep.mark.list", json!({})).unwrap()["marks"].as_array().unwrap().len(), 1);
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

    const PNG_A: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 6, 0, 0, 0, 114, 182, 13, 36, 0, 0, 0, 17, 73, 68,
        65, 84, 120, 156, 99, 56, 33, 39, 215, 0, 194, 12, 48, 6, 0, 56, 82, 6, 17, 216, 232, 162, 145, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    const PNG_B: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 6, 0, 0, 0, 114, 182, 13, 36, 0, 0, 0, 17, 73, 68,
        65, 84, 120, 156, 99, 144, 147, 59, 241, 31, 132, 25, 96, 12, 0, 65, 228, 8, 13, 149, 140, 250, 4, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
        130,
    ];

    /// Image A placed at `rect` with nothing selected → its id.
    fn put(app: &mut VectorcraftApp, rect: [f64; 4]) -> u64 {
        app.run("select.set", json!({"ids": []})).unwrap();
        place_file(app, &protocol::place(Some("a.png".into()), Some(PNG_A.to_vec()), Some(rect.to_vec())).unwrap()).unwrap()[0]
    }

    #[test]
    fn selection_counts_copies_of_one_image_by_printed_size_and_copies_replace_them_in_one_undo_step() {
        let mut app = app();
        assert_eq!(selection(&app), protocol::Selection::default());
        let (a, b) = (put(&mut app, [0.0, 0.0, 20.0, 20.0]), put(&mut app, [40.0, 0.0, 20.0, 20.0]));
        let _bigger = put(&mut app, [0.0, 40.0, 40.0, 40.0]);
        app.run("select.set", json!({"ids": [a]})).unwrap();
        assert_eq!(selection(&app), protocol::Selection { image: true, copies: 1, other_sizes: 1 });
        // A path is not an image; two images are not one.
        app.run("select.set", json!({"ids": [a, b]})).unwrap();
        assert_eq!(selection(&app), protocol::Selection::default());
        app.run("select.set", json!({"ids": [a]})).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        let undo = app.session.active().unwrap().history.undo.len();
        let place = protocol::place_copies(Some("b.png".into()), Some(PNG_B.to_vec()), None, Some(true)).unwrap();
        let placed = place_file(&mut app, &place).unwrap();
        assert_eq!(placed.len(), 2, "the selected image and its one same-size copy");
        let st = app.session.active().unwrap();
        assert_eq!(st.history.undo.len(), undo + 1, "one undo step");
        assert_eq!(st.selection.objects.len(), 2, "what was placed is selected");
        // The bigger copy was left alone, and is now the only image showing A.
        let mut keys = vec![];
        st.doc.walk(|n| {
            if let vectorcraft_doc::NodeKind::Image(im) = &n.kind {
                keys.push(im.key.clone())
            }
        });
        assert_eq!(keys.len(), 3);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc, before);
        assert!(protocol::place_copies(Some("b.png".into()), Some(PNG_B.to_vec()), Some(vec![0.0, 0.0, 1.0, 1.0]), Some(true)).is_err());
    }

    /// A 4 × 1 RGBA PNG: clear, soft below and at the default level, opaque.
    fn soft_png() -> Vec<u8> {
        let img = image::RgbaImage::from_raw(4, 1, vec![9, 8, 7, 0, 1, 2, 3, 127, 4, 5, 6, 128, 7, 7, 7, 255]).unwrap();
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn solidify_takes_the_selected_image_and_its_copies_in_one_undo_step() {
        let mut app = app();
        let png = soft_png();
        let put = |app: &mut VectorcraftApp, rect: [f64; 4]| {
            app.run("select.set", json!({"ids": []})).unwrap();
            place_file(app, &protocol::place(Some("s.png".into()), Some(png.clone()), Some(rect.to_vec())).unwrap()).unwrap()[0]
        };
        let (a, b) = (put(&mut app, [0.0, 0.0, 40.0, 10.0]), put(&mut app, [0.0, 20.0, 40.0, 10.0]));
        let one = protocol::solidify(None, None).unwrap();
        // Nothing selected, several objects selected: refused.
        app.run("select.set", json!({"ids": []})).unwrap();
        assert!(solidify(&mut app, &one).unwrap_err().contains("select one image"));
        app.run("select.all", json!({})).unwrap();
        assert!(solidify(&mut app, &one).unwrap_err().contains("select one image"));
        app.run("select.set", json!({"ids": [a]})).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        let undo = app.session.active().unwrap().history.undo.len();
        let done = solidify(&mut app, &one).unwrap();
        assert_eq!(done, Solidified { ids: vec![a], form: "alpha".into(), level: 128, soft: 2, clear: 2, opaque: 2 });
        assert_eq!(app.session.active().unwrap().history.undo.len(), undo + 1);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc, before);
        let all = protocol::solidify(Some(Some(200.0)), Some(Some(true))).unwrap();
        let done = solidify(&mut app, &all).unwrap();
        assert_eq!((done.ids, done.clear, done.opaque), (vec![a, b], 3, 1));
        assert_eq!(app.session.active().unwrap().history.undo.len(), undo + 1, "copies too in one undo step");
    }
}
