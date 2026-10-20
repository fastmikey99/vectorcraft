use super::*;
use vectorcraft_geom::shapes;

fn session(color: Color) -> Session {
    let mut d = Document::new(30.0, 30.0);
    let layer = d.layers[0].id;
    d.node_mut(layer).unwrap().name = Some("Artwork".into());
    let n = Node::path(d.alloc_id(), shapes::rectangle(Rect::new(5.0, 5.0, 25.0, 25.0)), Appearance::basic(Paint::solid(color), Paint::None, 0.0));
    d.insert(Some(layer), 0, n).unwrap();
    let mut s = Session::new();
    s.add_document(d, None);
    s
}

#[test]
fn external_only_and_nearest_excluded_feature() {
    let mut art = vec![false; 81];
    for y in 2..7 {
        for x in 2..7 {
            art[y * 9 + x] = true;
        }
    }
    art[4 * 9 + 4] = false;
    let mut allowed = art.clone();
    allowed[2 * 9 + 4] = false;
    let band = pixels::band(&art, &allowed, 9, 9, 2.0).unwrap();
    assert!(band[4 * 9 + 4].is_some(), "blank counter is outside the art union");
    assert_eq!(band[4 * 9 + 3], None, "ink-to-ink interior is never trapped");
    assert_eq!(band[9 + 4], None, "nearest excluded art wins over nearby eligible art");
    assert_eq!(band[9 + 3], Some(2 * 9 + 3));
    assert_eq!(band[0], None, "Euclidean radius, not a square dilation");
}

#[test]
fn rich_black_and_white_produce_no_trap() {
    for color in [Color::WHITE, Color::BLACK, Color::cmyk(0.73, 0.68, 0.63, 0.76)] {
        let mut s = session(color);
        assert_eq!(s.execute(C, &json!({"blackThreshold":0.7})).unwrap()["pixels"], 0);
        assert_eq!(s.doc().unwrap().doc.layers.len(), 1);
    }
}

#[test]
fn replacement_undo_and_pdf_mask() {
    let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
    assert!(s.execute(C, &json!({})).is_err());
    let reply = s.execute(C, &json!({"blackThreshold":0.7})).unwrap();
    assert!(reply["pixels"].as_u64().unwrap() > 0);
    assert_eq!(s.doc().unwrap().doc.layers[0].name.as_deref(), Some("Auto Trap-1"));
    let bytes = vectorcraft_pdf::export(&s.doc().unwrap().doc, &Default::default()).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("/SMask"));
    let read = vectorcraft_pdf::import(&bytes).unwrap();
    assert!(read.images.values().any(ImageBlob::is_cmyk));
    s.execute(C, &json!({"blackThreshold":0.7})).unwrap();
    assert_eq!(s.doc().unwrap().doc.layers.len(), 2);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.doc().unwrap().doc.layers.len(), 2);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.doc().unwrap().doc.layers.len(), 1);
}

#[test]
fn invalid_grid_rejected() {
    assert!(pixels::band(&[], &[], 0, 0, 4.0).is_err());
    assert!(pixels::band(&[true], &[true], 1, 1, f64::NAN).is_err());
}

#[test]
fn spot_inks_stay_on_their_plate_and_excluded_names_block_them() {
    let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
    {
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        d.swatches.push(vectorcraft_color::Swatch {
            name: "Synthetic spot".into(),
            paint: Paint::solid(Color::cmyk(1.0, 0.0, 0.0, 0.0)),
            global: true,
            spot: true,
        });
        vectorcraft_doc::inks::visit_node_colors(Arc::make_mut(&mut d.layers[0]), &mut |_, name, _| *name = Some("Synthetic spot".into()));
    }
    s.execute(C, &json!({"blackThreshold":0.7})).unwrap();
    let d = &s.doc().unwrap().doc;
    let image = d.images.values().find(|i| !i.spot_names().is_empty()).unwrap();
    assert!(image.cmyk().unwrap().data.iter().all(|&v| v == 0));
    assert!(image.spot_inks().unwrap().data.contains(&255));
    let bytes = vectorcraft_pdf::export(d, &Default::default()).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("/DeviceN"));
    assert!(String::from_utf8_lossy(&bytes).contains("/SMask"));
    let read = vectorcraft_pdf::import(&bytes).unwrap();
    assert!(read.images.values().any(|i| i.spot_names() == ["Synthetic spot"]));
    {
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        let art = d.layers.iter_mut().find(|n| n.name.as_deref() == Some("Artwork")).unwrap();
        vectorcraft_doc::inks::visit_node_colors(Arc::make_mut(art), &mut |_, _, tint| *tint = 0.0);
    }
    assert_eq!(s.execute(C, &json!({"blackThreshold":0.7})).unwrap()["pixels"], 0, "zero-tint ink");
    for name in ["NEON-Test", "M-Test"] {
        let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        Arc::make_mut(&mut Arc::make_mut(&mut d.layers[0]).children_mut().unwrap()[0]).name = Some(name.into());
        assert!(s.execute(C, &json!({"blackThreshold":0.7})).is_err());
    }
}

#[test]
fn cancellation_does_not_replace_an_existing_trap() {
    let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
    s.execute(C, &json!({"blackThreshold":0.7})).unwrap();
    let before = s.doc().unwrap().doc.clone();
    let history = s.doc().unwrap().history.undo.len();
    let token = crate::cancel::CancelToken::new();
    token.cancel();
    let result = crate::cancel::scope(&token, None, || s.execute(C, &json!({"blackThreshold":0.7})));
    assert!(matches!(result, Err(crate::EngineError::Cancelled)));
    assert!(Arc::ptr_eq(&before, &s.doc().unwrap().doc));
    assert_eq!(s.doc().unwrap().history.undo.len(), history);
}

#[test]
fn pal_ink_epsilon_and_k_threshold_are_independent_of_brightness() {
    for (color, expected) in [
        (Color::cmyk(0.003, 0.004, 0.004, 0.0), false),
        (Color::cmyk(0.005, 0.0, 0.0, 0.0), true),
        (Color::cmyk(0.4, 0.96, 0.38, 0.5), true),
        (Color::cmyk(0.0, 0.0, 0.0, 0.7), false),
        (Color::cmyk(0.0, 0.0, 0.0, 0.699), true),
    ] {
        let mut s = session(color);
        assert_eq!(s.execute(C, &json!({"blackThreshold":0.7})).unwrap()["pixels"].as_u64().unwrap() > 0, expected);
    }
    for name in ["NO WHP", "NO WHP OR POWDER", "NO POWDER", "NO WHP detail", "neon", "m-test"] {
        let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        Arc::make_mut(&mut Arc::make_mut(&mut d.layers[0]).children_mut().unwrap()[0]).name = Some(name.into());
        assert!(s.execute(C, &json!({"blackThreshold":0.7})).unwrap()["pixels"].as_u64().unwrap() > 0);
    }
}

#[test]
fn spot_tint_uses_pal_epsilon_and_black_uses_the_alternate() {
    let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
    {
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        d.swatches.push(vectorcraft_color::Swatch {
            name: "Synthetic alternate".into(),
            paint: Paint::solid(Color::cmyk(0.0, 0.0, 0.0, 0.8)),
            global: true,
            spot: true,
        });
        vectorcraft_doc::inks::visit_node_colors(Arc::make_mut(&mut d.layers[0]), &mut |_, name, _| *name = Some("Synthetic alternate".into()));
    }
    assert_eq!(s.execute(C, &json!({"blackThreshold":0.7})).unwrap()["pixels"], 0);
    for (tint, expected) in [(0.004, false), (0.005, true)] {
        {
            let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
            d.swatches.iter_mut().find(|s| s.name == "Synthetic alternate").unwrap().paint = Paint::solid(Color::cmyk(1.0, 0.0, 0.0, 0.0));
            let art = d.layers.iter_mut().find(|n| n.name.as_deref() == Some("Artwork")).unwrap();
            vectorcraft_doc::inks::visit_node_colors(Arc::make_mut(art), &mut |_, _, t| *t = tint);
        }
        assert_eq!(s.execute(C, &json!({"blackThreshold":0.7})).unwrap()["pixels"].as_u64().unwrap() > 0, expected);
    }
}
