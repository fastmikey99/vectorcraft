//! Line weight check (`lineweight.check`, `lineweight.clear`): thin parts and narrow gaps of the
//! printing art's silhouette, and the non-printing layer that marks them.

use serde_json::{Value, json};
use vectorcraft_doc::{Node, NodeKind};

use super::*;

const MM: f64 = 72.0 / 25.4;

fn session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({"width": 400, "height": 300})).unwrap();
    s
}

/// A black-filled, unstroked rectangle, in millimetres from (100 pt, 100 pt).
fn rect(s: &mut Session, x: f64, y: f64, w: f64, h: f64) -> u64 {
    let r = s.execute("shape.rectangle", &json!({"x": 100.0 + x * MM, "y": 100.0 + y * MM, "width": w * MM, "height": h * MM})).unwrap();
    let id = r["id"].as_u64().unwrap();
    s.execute("select.set", &json!({"ids": [id]})).unwrap();
    s.execute("paint.setFill", &json!({"color": "#000000"})).unwrap();
    s.execute("paint.setStroke", &json!({"none": true})).unwrap();
    id
}

/// The 1 mm square with a 0.1 mm wide, 2 mm long line running off its right side.
fn square_and_hairline(s: &mut Session) -> (u64, u64) {
    (rect(s, 0.0, 0.0, 1.0, 1.0), rect(s, 1.0, 0.45, 2.0, 0.1))
}

fn check(s: &mut Session, p: Value) -> Value {
    s.execute("lineweight.check", &p).unwrap()
}

fn counts(r: &Value) -> (u64, u64) {
    (r["thin"]["count"].as_u64().unwrap(), r["gaps"]["count"].as_u64().unwrap())
}

/// The top-level layers named like the check's.
fn check_layers(s: &Session) -> Vec<Node> {
    s.doc().unwrap().doc.layers.iter().filter(|l| l.name.as_deref() == Some(cmd::lineweight::LAYER_NAME)).map(|l| (**l).clone()).collect()
}

#[test]
fn a_hairline_touching_a_square_is_thin_only_at_a_minimum_wider_than_it() {
    let mut s = session();
    square_and_hairline(&mut s);
    let r = check(&mut s, json!({"min": "0.2 mm"}));
    assert_eq!(counts(&r), (1, 0), "{r}");
    assert_eq!(r["checkedObjects"], 2);
    assert_eq!(r["complete"], true);
    assert!((r["minMm"].as_f64().unwrap() - 0.2).abs() < 1e-9);
    // The region is the line: from about where it leaves the square to its end.
    let b: Vec<f64> = r["thin"]["regions"][0]["bounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(b[0] > 100.0 + 0.8 * MM && (b[2] - (100.0 + 3.0 * MM)).abs() < 0.05, "{b:?}");
    assert!((b[3] - b[1] - 0.1 * MM).abs() < 0.05, "{b:?}");
    assert_eq!(counts(&check(&mut s, json!({"min": "0.05 mm"}))), (0, 0));
    // Points work as well as a length with a unit, and the default is 0.2 mm.
    assert_eq!(counts(&check(&mut s, json!({"min": 0.2 * MM}))), (1, 0));
    assert_eq!(counts(&check(&mut s, json!({}))), (1, 0));
}

#[test]
fn two_squares_a_tenth_of_a_millimetre_apart_leave_one_narrow_gap() {
    let mut s = session();
    rect(&mut s, 0.0, 0.0, 1.0, 1.0);
    rect(&mut s, 1.1, 0.0, 1.0, 1.0);
    assert_eq!(counts(&check(&mut s, json!({"min": "0.2 mm"}))), (0, 1));
    assert_eq!(counts(&check(&mut s, json!({"min": "0.2 mm", "gaps": false}))), (0, 0));
    assert_eq!(counts(&check(&mut s, json!({"min": "0.05 mm"}))), (0, 0));
}

#[test]
fn a_clean_square_has_nothing_to_report() {
    let mut s = session();
    rect(&mut s, 0.0, 0.0, 5.0, 5.0);
    let r = check(&mut s, json!({"min": "0.3 mm"}));
    assert_eq!(counts(&r), (0, 0), "{r}");
    assert_eq!((r["checkedObjects"].as_u64(), r["skippedImages"].as_u64()), (Some(1), Some(0)));
    assert!(r.get("marked").is_none());
}

#[test]
fn strokes_count_as_the_area_they_paint() {
    let mut s = session();
    let line = s.execute("shape.line", &json!({"x1": 100, "y1": 100, "x2": 200, "y2": 100})).unwrap()["id"].as_u64().unwrap();
    s.execute("select.set", &json!({"ids": [line]})).unwrap();
    s.execute("paint.setFill", &json!({"none": true})).unwrap();
    s.execute("paint.setStroke", &json!({"color": "#000000"})).unwrap();
    s.execute("stroke.set", &json!({"weight": 0.1 * MM})).unwrap();
    let r = check(&mut s, json!({"min": "0.2 mm"}));
    assert_eq!((counts(&r), r["checkedObjects"].as_u64()), ((1, 0), Some(1)), "{r}");
    s.execute("stroke.set", &json!({"weight": 1.0 * MM})).unwrap();
    assert_eq!(counts(&check(&mut s, json!({"min": "0.2 mm"}))), (0, 0));
    // A dashed line with gaps between its dashes narrower than the minimum.
    s.execute("stroke.set", &json!({"dash": [10, 0.1 * MM]})).unwrap();
    let (thin, gaps) = counts(&check(&mut s, json!({"min": "0.2 mm"})));
    assert!(thin == 0 && gaps >= 5, "{thin} {gaps}");
}

#[test]
fn type_is_checked_as_its_outlines() {
    let mut s = session();
    let t = s.execute("text.create", &json!({"x": 100, "y": 150, "text": "Hi", "size": 6})).unwrap()["id"].as_u64().unwrap();
    let NodeKind::Text(text) = &s.doc().unwrap().doc.node(NodeId(t)).unwrap().kind else { panic!() };
    if vectorcraft_render::effects::outline_text(&Node::new(NodeId(t), NodeKind::Text(text.clone())))
        .is_none_or(|g| g.children().is_none_or(|c| c.is_empty()))
    {
        eprintln!("no font to lay type out with (CRAFT_FONTS empty): skipped");
        return;
    }
    let as_type = check(&mut s, json!({"min": "0.2 mm"}));
    assert_eq!(as_type["checkedObjects"], 1, "{as_type}");
    assert!(as_type["segments"].as_u64().unwrap() > 8);
    s.execute("select.set", &json!({"ids": [t]})).unwrap();
    s.execute("type.createOutlines", &json!({})).unwrap();
    let outlined = check(&mut s, json!({"min": "0.2 mm"}));
    assert_eq!(counts(&as_type), counts(&outlined), "{as_type} {outlined}");
}

#[test]
fn hidden_template_and_non_printing_art_and_guides_are_left_out() {
    let mut s = session();
    rect(&mut s, 0.0, 0.0, 5.0, 5.0);
    // A hidden hairline.
    let hidden = rect(&mut s, 10.0, 0.0, 0.1, 3.0);
    s.execute("select.set", &json!({"ids": [hidden]})).unwrap();
    s.execute("object.hide", &json!({})).unwrap();
    // A hairline made a guide.
    let guide = rect(&mut s, 20.0, 0.0, 0.1, 3.0);
    s.execute("select.set", &json!({"ids": [guide]})).unwrap();
    s.execute("view.guides.make", &json!({})).unwrap();
    // Hairlines on a non-printing layer and on a template layer.
    for opts in [json!({"printable": false}), json!({"template": true})] {
        let layer = s.execute("layer.new", &json!({"top": true})).unwrap()["id"].as_u64().unwrap();
        s.doc_mut().unwrap().active_layer = Some(NodeId(layer));
        rect(&mut s, 30.0, 0.0, 0.1, 3.0);
        let mut p = opts.clone();
        p["id"] = json!(layer);
        s.execute("layer.setProps", &p).unwrap();
    }
    let r = check(&mut s, json!({"min": "0.2 mm"}));
    assert_eq!((counts(&r), r["checkedObjects"].as_u64()), ((0, 0), Some(1)), "{r}");
    // Asked for by id, hidden art still isn't checked.
    let r = check(&mut s, json!({"min": "0.2 mm", "ids": [hidden]}));
    assert_eq!((counts(&r), r["checkedObjects"].as_u64()), ((0, 0), Some(0)), "{r}");
}

#[test]
fn art_clipped_away_is_left_out() {
    let mut s = session();
    let (square, line) = square_and_hairline(&mut s);
    // A clipping path round the square and the first half millimetre of the line.
    let clip = rect(&mut s, -0.5, -0.5, 2.0, 2.0);
    s.execute("select.set", &json!({"ids": [square, line, clip]})).unwrap();
    s.execute("object.clippingMask.make", &json!({})).unwrap();
    let r = check(&mut s, json!({"min": "0.2 mm"}));
    // What is left of the line past the square is 0.1 × 0.5 mm, longer than the minimum.
    assert_eq!(counts(&r), (1, 0), "{r}");
    let b: Vec<f64> = r["thin"]["regions"][0]["bounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!((b[2] - (100.0 + 1.5 * MM)).abs() < 0.05, "clipped at the clipping path's edge: {b:?}");
}

#[test]
fn images_are_skipped_and_counted() {
    let mut s = session();
    rect(&mut s, 0.0, 0.0, 5.0, 5.0);
    let mut png = vec![];
    image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    s.execute("file.place", &json!({"name": "a.png", "dataBase64": vectorcraft_format::base64_encode(&png)})).unwrap();
    let r = check(&mut s, json!({"min": "0.2 mm"}));
    assert_eq!((r["skippedImages"].as_u64(), r["checkedObjects"].as_u64(), counts(&r)), (Some(1), Some(1), (0, 0)), "{r}");
}

#[test]
fn ids_limit_the_check_to_those_objects() {
    let mut s = session();
    let (square, line) = square_and_hairline(&mut s);
    rect(&mut s, 10.0, 0.0, 0.1, 3.0);
    assert_eq!(counts(&check(&mut s, json!({"min": "0.2 mm"}))), (2, 0));
    let r = check(&mut s, json!({"min": "0.2 mm", "ids": [square, line]}));
    assert_eq!((counts(&r), r["checkedObjects"].as_u64()), ((1, 0), Some(2)), "{r}");
    assert_eq!(counts(&check(&mut s, json!({"min": "0.2 mm", "ids": [square]}))), (0, 0));
}

#[test]
fn bad_params_are_refused() {
    let mut s = session();
    rect(&mut s, 0.0, 0.0, 1.0, 1.0);
    for p in [
        json!({"min": "thin"}),
        json!({"min": 0}),
        json!({"min": -1}),
        json!({"min": "2 in"}),
        json!({"timeLimit": 0}),
        json!({"maxRegions": -1}),
        json!({"ids": [999_999]}),
    ] {
        assert!(matches!(s.execute("lineweight.check", &p), Err(EngineError::BadParams { .. })), "{p}");
        assert!(check_params("lineweight.check", &p).is_ok(), "{p}: the keys are the documented ones");
    }
    assert!(check_params("lineweight.check", &json!({"minimum": 1})).is_err());
}

#[test]
fn listed_regions_and_their_outlines_are_capped_but_the_counts_are_not() {
    let mut s = session();
    for i in 0..5 {
        rect(&mut s, 3.0 * f64::from(i), 0.0, 0.1, 2.0);
    }
    let r = check(&mut s, json!({"min": "0.2 mm", "maxRegions": 2, "paths": true}));
    assert_eq!(r["thin"]["count"], 5);
    let regions = r["thin"]["regions"].as_array().unwrap();
    assert_eq!(regions.len(), 2);
    assert!(regions[0]["d"].as_str().unwrap().starts_with('M'));
    assert!(check(&mut s, json!({"min": "0.2 mm"}))["thin"]["regions"][0].get("d").is_none());
}

#[test]
fn marks_go_on_a_locked_non_printing_template_layer_on_top_replaced_on_rerun_and_cleared() {
    let mut s = session();
    square_and_hairline(&mut s);
    rect(&mut s, 0.0, 3.0, 1.0, 1.0);
    rect(&mut s, 1.1, 3.0, 1.0, 1.0);
    let undo_before = s.doc().unwrap().history.undo.len();
    let r = check(&mut s, json!({"min": "0.2 mm", "mark": true}));
    assert_eq!(counts(&r), (1, 1), "{r}");
    let layers = check_layers(&s);
    assert_eq!(layers.len(), 1);
    let layer = &layers[0];
    assert_eq!(r["marked"].as_u64(), Some(layer.id.0));
    assert_eq!(s.doc().unwrap().doc.layers.last().unwrap().id, layer.id, "on top of the others");
    let NodeKind::Layer { template, printable, children, .. } = &layer.kind else { panic!() };
    assert!(*template && !*printable && layer.locked);
    let names: Vec<_> = children.iter().map(|g| (g.name.clone().unwrap(), g.children().unwrap().len())).collect();
    assert_eq!(names, [("Thin parts".to_string(), 1), ("Narrow gaps".to_string(), 1)]);
    assert_eq!(s.doc().unwrap().history.undo.len(), undo_before + 1, "one undo step");
    // The art itself is checked without the marks, which are left out as a template layer.
    let again = check(&mut s, json!({"min": "0.2 mm", "mark": true}));
    assert_eq!(counts(&again), (1, 1));
    let layers = check_layers(&s);
    assert_eq!(layers.len(), 1, "replaced, not added");
    assert_ne!(layers[0].id, layer.id);
    // A check that marks nothing removes the old marks.
    check(&mut s, json!({"min": "0.01 mm", "mark": true}));
    assert!(check_layers(&s).is_empty());
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(check_layers(&s).len(), 1, "undo brings the marks back");
    assert_eq!(s.execute("lineweight.clear", &json!({})).unwrap()["removed"], 1);
    assert!(check_layers(&s).is_empty());
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(check_layers(&s).len(), 1);
    s.execute("edit.redo", &json!({})).unwrap();
    // Clearing nothing changes nothing.
    let undo = s.doc().unwrap().history.undo.len();
    assert_eq!(s.execute("lineweight.clear", &json!({})).unwrap()["removed"], 0);
    assert_eq!(s.doc().unwrap().history.undo.len(), undo);
}

#[test]
fn an_unmarked_check_changes_nothing() {
    let mut s = session();
    square_and_hairline(&mut s);
    let undo = s.doc().unwrap().history.undo.len();
    let before = s.doc().unwrap().doc.clone();
    check(&mut s, json!({"min": "0.2 mm"}));
    assert_eq!(s.doc().unwrap().history.undo.len(), undo);
    assert!(Arc::ptr_eq(&before, &s.doc().unwrap().doc) || *before == *s.doc().unwrap().doc);
}

/// Leaf paths of a document (what an export drew).
fn leaf_paths(s: &Session) -> usize {
    let mut n = 0;
    for l in &s.doc().unwrap().doc.layers {
        l.walk(&mut |c| n += usize::from(matches!(c.kind, NodeKind::Path { .. })));
    }
    n
}

#[test]
fn the_marks_are_never_exported() {
    let mut s = session();
    square_and_hairline(&mut s);
    let export = |s: &mut Session, format: &str| s.execute("document.serialize", &json!({"format": format})).unwrap();
    let plain_pdf = export(&mut s, "pdf");
    check(&mut s, json!({"min": "0.2 mm", "mark": true}));
    assert_eq!(check_layers(&s).len(), 1);
    // PNG: no magenta pixel.
    let png = vectorcraft_format::base64_decode(export(&mut s, "png")["dataBase64"].as_str().unwrap()).unwrap();
    let img = image::load_from_memory(&png).unwrap().to_rgba8();
    assert!(!img.pixels().any(|p| p[0] > 200 && p[1] < 80 && p[2] > 200 && p[3] > 0), "a mark was drawn in the PNG");
    // SVG: nothing of the layer.
    let svg = export(&mut s, "svg")["text"].as_str().unwrap().to_string();
    assert!(!svg.contains("Lineweight") && !svg.to_ascii_lowercase().contains("#f0f") && !svg.to_ascii_lowercase().contains("#ff00ff"), "{svg}");
    // PDF: its page, read again (not the native document a PDF carries for editing, which keeps
    // the check layer as a native save does), holds as many paths as the PDF written before the
    // marks.
    let marked_pdf = export(&mut s, "pdf");
    let count = |data: &Value| {
        let mut t = Session::new();
        let r = t.execute("document.open", &json!({"name": "a.pdf", "dataBase64": data["dataBase64"], "editingData": false, "pages": "1"})).unwrap();
        assert_eq!(r["restored"], false, "read from the page");
        leaf_paths(&t)
    };
    assert_eq!(count(&marked_pdf), count(&plain_pdf));
}
