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
    let band = pixels::band(&art, &allowed, &pixels::close_union(&art, 9, 9), 9, 9, 2.0).unwrap();
    assert_eq!(band[4 * 9 + 4], None, "one-pixel hole is sealed before finding external edges");
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
    let trap = &s.doc().unwrap().doc.layers[0];
    let clip = &trap.children().unwrap()[0].children().unwrap()[0];
    assert!(clip.appearance.fill_paint().is_none(), "a clip must not add black ink outside its masked image after cleanup");
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
    assert!(pixels::band(&[], &[], &[], 0, 0, 4.0).is_err());
    assert!(pixels::band(&[true], &[true], &[true], 1, 1, f64::NAN).is_err());
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
    assert!(image.spot_inks().unwrap().unwrap().data.contains(&255));
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
    {
        let name = "NEON-Test";
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

#[test]
fn union_closing_seals_seams_but_retains_counters_and_colour_ownership() {
    let (w, h) = (25, 21);
    let mut art = vec![false; w * h];
    for y in 3..18 {
        for x in 3..22 {
            art[y * w + x] = true;
        }
    }
    // Adjacent painted pieces have a one-pixel hairline gap.
    for y in 3..18 {
        art[y * w + 8] = false;
    }
    // A real counter remains part of the external art boundary.
    for y in 7..14 {
        for x in 12..19 {
            art[y * w + x] = false;
        }
    }
    let mut allowed = art.clone();
    for y in 3..18 {
        allowed[y * w + 21] = false;
    }
    let band = pixels::band(&art, &allowed, &pixels::close_union(&art, w, h), w, h, 2.0).unwrap();
    for y in 3..18 {
        assert_eq!(band[y * w + 8], None);
    }
    assert!(band[7 * w + 14].is_some());
    assert_eq!(band[10 * w + 15], None, "counter centre exceeds the trap radius");
    assert_eq!(band[10 * w + 22], None, "excluded outer edge keeps nearest ownership");
    assert!(band[10 * w + 2].is_some());
    for source in band.iter().flatten() {
        assert!(art[*source as usize], "never sample a fabricated seam pixel");
    }
}

#[test]
fn trap_width_is_measured_from_pixel_cells_not_sample_centres() {
    let mut art = vec![false; 15 * 15];
    art[7 * 15 + 7] = true;
    let band = pixels::band(&art, &art, &art, 15, 15, 0.2 * 600.0 / 25.4).unwrap();
    assert_eq!(band[7 * 15 + 12], Some(7 * 15 + 7), "fifth centre is only 4.5 pixels beyond the cell edge");
    assert_eq!(band[7 * 15 + 13], None, "sixth centre exceeds 0.2 mm");
    assert_eq!(band[11 * 15 + 11], None, "corners use Euclidean distance, not square dilation");
    let narrower = pixels::band(&art, &art, &art, 15, 15, 4.0).unwrap();
    assert_eq!(narrower[7 * 15 + 12], None);
}

#[test]
fn vector_occupancy_unites_adjacent_fills_and_preserves_small_counters() {
    use vectorcraft_geom::FillRule;
    let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
    let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
    let layer = d.layers[0].id;
    let n = Node::path(d.alloc_id(), shapes::rectangle(Rect::new(25.0, 5.0, 30.0, 25.0)), Appearance::default_art());
    // Remove the default stroke: this case measures fill union only.
    let mut n = n;
    n.appearance = Appearance::basic(Paint::solid(Color::WHITE), Paint::None, 0.0).into();
    d.insert(Some(layer), 1, n).unwrap();
    let path = union::artwork(d).unwrap().unwrap();
    assert!((vectorcraft_pathops::area(&path, FillRule::NonZero) - 500.0).abs() < 1e-6);

    let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
    let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
    let n = Arc::make_mut(&mut Arc::make_mut(&mut d.layers[0]).children_mut().unwrap()[0]);
    if let NodeKind::Path { path, rule, .. } = &mut n.kind {
        path.subpaths.extend(shapes::rectangle(Rect::new(10.0, 10.0, 10.05, 10.05)).subpaths);
        *rule = FillRule::EvenOdd;
    }
    let path = union::artwork(d).unwrap().unwrap();
    assert!((vectorcraft_pathops::area(&path, FillRule::NonZero) - 399.9975).abs() < 1e-6, "vector counters survive below one pixel");
}

#[test]
fn vector_occupancy_obeys_clips_and_leaves_transparency_to_the_renderer() {
    use vectorcraft_geom::FillRule;
    let mut d = Document::new(30.0, 30.0);
    let layer = d.layers[0].id;
    let clip = Node::path(d.alloc_id(), shapes::rectangle(Rect::new(0.0, 0.0, 10.0, 10.0)), Appearance::default());
    let fill =
        Node::path(d.alloc_id(), shapes::rectangle(Rect::new(5.0, 5.0, 15.0, 15.0)), Appearance::basic(Paint::solid(Color::BLACK), Paint::None, 0.0));
    let mut group = Node::group(d.alloc_id(), vec![Arc::new(clip), Arc::new(fill)]);
    if let NodeKind::Group { clip, .. } = &mut group.kind {
        *clip = true;
    }
    let id = group.id;
    d.insert(Some(layer), 0, group).unwrap();
    let path = union::artwork(&d).unwrap().unwrap();
    assert!((vectorcraft_pathops::area(&path, FillRule::NonZero) - 25.0).abs() < 1e-6);
    d.node_mut(id).unwrap().opacity = 0.5;
    assert!(union::artwork(&d).unwrap().is_none());
}

#[test]
fn flat_band_ink_uses_the_nearest_vector_boundary_and_keeps_exclusions() {
    let mut d = Document::new(30.0, 30.0);
    let layer = d.layers[0].id;
    for (rect, color) in
        [(Rect::new(0.0, 0.0, 10.0, 9.9), Color::cmyk(1.0, 0.0, 0.0, 0.0)), (Rect::new(10.6, 0.0, 20.0, 9.9), Color::cmyk(0.0, 1.0, 0.0, 0.0))]
    {
        let mut path = shapes::rectangle(rect);
        for subpath in &mut path.subpaths {
            subpath.closed = false;
        }
        let node = Node::path(d.alloc_id(), path, Appearance::basic(Paint::solid(color), Paint::None, 0.0));
        d.insert(Some(layer), d.node(layer).unwrap().children().unwrap().len(), node).unwrap();
    }
    let region = Rect::new(10.49 - 0.5 / SCALE, 5.0 - 0.5 / SCALE, 11.0, 11.0);
    let mut nearest = vec![Some(0)]; // A raster tie can select a pixel of the more distant cyan object.
    let colours = colour::samples(&d, &mut nearest, 1, region, 0.7).unwrap().unwrap();
    let mut process = [0; 4];
    colours.write(&vectorcraft_doc::inks::plates(&d), &mut process, &mut []);
    assert_eq!(process, [0, 255, 0, 0]);
    let last = d.node(layer).unwrap().children().unwrap()[1].id;
    d.node_mut(last).unwrap().appearance = Appearance::basic(Paint::solid(Color::cmyk(0.0, 0.0, 0.0, 0.8)), Paint::None, 0.0).into();
    let _ = colour::samples(&d, &mut nearest, 1, region, 0.7).unwrap().unwrap();
    assert_eq!(nearest, [None], "the closer excluded boundary cannot borrow cyan ink");
}

#[test]
fn complex_appearances_keep_renderer_colour_sampling() {
    let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
    let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
    let node = Arc::make_mut(&mut Arc::make_mut(&mut d.layers[0]).children_mut().unwrap()[0]);
    if let vectorcraft_doc::AppearanceItem::Fill(f) = &mut node.appearance.items[0] {
        f.overprint = true;
    }
    let mut nearest = vec![Some(0)];
    assert!(colour::samples(d, &mut nearest, 1, Rect::new(0.0, 0.0, 1.0, 1.0), 0.7).unwrap().is_none());
    assert_eq!(nearest, [Some(0)]);
}

#[test]
fn gradient_eligibility_requires_a_proven_constant_ink_predicate() {
    use vectorcraft_color::gradient::{Gradient, GradientPaint, GradientStop};
    let ramp = |a, b| GradientPaint::new(Gradient { stops: vec![GradientStop::new(0.0, a), GradientStop::new(1.0, b)], ..Default::default() });
    let g = ramp(Color::cmyk(0.2, 0.0, 0.0, 0.0), Color::cmyk(0.8, 0.0, 0.0, 0.0));
    assert_eq!(gradient_eligibility(&g, 0.7), Some(true));
    assert_eq!(gradient_eligibility(&ramp(Color::cmyk(0.0, 0.0, 0.0, 0.8), Color::cmyk(0.0, 0.0, 0.0, 0.9)), 0.7), Some(false));
    assert_eq!(gradient_eligibility(&ramp(Color::cmyk(0.0, 0.0, 0.0, 0.6), Color::cmyk(0.0, 0.0, 0.0, 0.8)), 0.7), None);
    // Both ends use ink, but the middle has less than epsilon on every channel.
    assert_eq!(gradient_eligibility(&ramp(Color::cmyk(0.006, 0.0, 0.0, 0.0), Color::cmyk(0.0, 0.006, 0.0, 0.0)), 0.7), None);
    let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
    {
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        let n = Arc::make_mut(&mut Arc::make_mut(&mut d.layers[0]).children_mut().unwrap()[0]);
        n.appearance = Appearance::basic(Paint::Gradient(Box::new(g)), Paint::None, 0.0).into();
    }
    assert!(s.execute(C, &json!({"blackThreshold":0.7})).unwrap()["pixels"].as_u64().unwrap() > 0);
    let d = &s.doc().unwrap().doc;
    let inks = d.images.values().find_map(ImageBlob::cmyk).unwrap();
    let cyan: std::collections::BTreeSet<_> = inks.data.as_chunks::<4>().0.iter().map(|p| p[0]).filter(|v| *v > 0).collect();
    assert!(cyan.len() > 10, "gradient samples remain a ramp rather than one flat tint");
}

#[test]
fn white_no_bleed_masks_suppress_coincident_edges_in_both_trap_outputs() {
    for task in [false, true] {
        for name in ["LIGHT NO BLEED", "NO WHP NO BLEED", "NO POWDER NO BLEED", "NO WHP OR POWDER NO BLEED"] {
            for front in [false, true] {
                let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
                s.edit("Trap", |d, _| {
                    let layer = d.layers[0].id;
                    let mut mask = Node::path(
                        d.alloc_id(),
                        shapes::rectangle(Rect::new(5.0, 5.0, 25.0, 25.0)),
                        Appearance::basic(Paint::solid(Color::cmyk(0.0, 0.0, 0.0, 0.0)), Paint::None, 0.0),
                    );
                    mask.name = Some(name.into());
                    d.insert(Some(layer), if front { usize::MAX } else { 0 }, mask)?;
                    Ok(())
                })
                .unwrap();
                let result = if task { trap_task(&mut s) } else { s.execute(C, &json!({"blackThreshold":0.7})) }.unwrap();
                if front {
                    assert_eq!(result["pixels"], 0);
                } else {
                    assert!(result["pixels"].as_u64().unwrap() > 0, "a white mask behind the art must not suppress visible ink");
                }
            }
        }
    }
}

#[test]
fn metallic_object_names_use_the_existing_ink_predicate() {
    for name in ["M-1", "M-2", "DK M-2", "M-28", "DK M-SPOT"] {
        let mut s = session(Color::cmyk(1.0, 0.0, 0.0, 0.0));
        let d = Arc::make_mut(&mut s.doc_mut().unwrap().doc);
        Arc::make_mut(&mut Arc::make_mut(&mut d.layers[0]).children_mut().unwrap()[0]).name = Some(name.into());
        assert!(s.execute(C, &json!({"blackThreshold":0.7})).unwrap()["pixels"].as_u64().unwrap() > 0);
    }
}

#[test]
fn coincident_and_partly_covered_outlines_get_visible_front_ink() {
    for front_rect in [Rect::new(0., 0., 10., 10.), Rect::new(5., 0., 10., 10.)] {
        for back_color in [Color::cmyk(1., 0., 0., 0.), Color::BLACK] {
            let mut doc = Document::new(20., 20.);
            let layer = doc.default_layer().unwrap();
            for (i, (r, c)) in [(Rect::new(0., 0., 10., 10.), back_color), (front_rect, Color::cmyk(0., 1., 0., 0.))].into_iter().enumerate() {
                let n = Node::path(doc.alloc_id(), shapes::rectangle(r), Appearance::basic(Paint::solid(c), Paint::None, 0.));
                doc.insert(Some(layer), i, n).unwrap();
            }
            let mut nearest = vec![Some(0)];
            let region = Rect::new(10.1 - 0.5 / SCALE, 5. - 0.5 / SCALE, 11., 6.);
            let samples = colour::samples(&doc, &mut nearest, 1, region, 0.7).unwrap().unwrap();
            let mut process = [0; 4];
            samples.write(&vectorcraft_doc::inks::plates(&doc), &mut process, &mut []);
            assert_eq!(nearest, [Some(0)]);
            assert_eq!(process, [0, 255, 0, 0]);
            if front_rect.x0 > 0. {
                let mut nearest = vec![Some(0)];
                let region = Rect::new(-0.1 - 0.5 / SCALE, 5. - 0.5 / SCALE, 1., 6.);
                let samples = colour::samples(&doc, &mut nearest, 1, region, 0.7).unwrap().unwrap();
                let mut process = [0; 4];
                samples.write(&vectorcraft_doc::inks::plates(&doc), &mut process, &mut []);
                if back_color == Color::BLACK {
                    assert_eq!(nearest, [None]);
                } else {
                    assert_eq!(process, [255, 0, 0, 0]);
                }
            }
        }
    }
}

#[test]
fn trap_ink_boundary_and_adjacent_floats_match_shared_presence() {
    let epsilon = vectorcraft_doc::inks::PRESENCE_EPSILON;
    for value in [f32::from_bits(epsilon.to_bits() - 1), epsilon, f32::from_bits(epsilon.to_bits() + 1)] {
        let doc = Document::new(10., 10.);
        let (_, eligible) = paint_ink(&doc, &Color::cmyk(value, 0., 0., 0.), None, 1., 0.7);
        assert_eq!(eligible, vectorcraft_doc::inks::present(value, epsilon));
    }
}

#[test]
fn adversarial_ownership_falls_back_and_cancellation_is_not_swallowed() {
    let mut d = Document::new(1000., 1000.);
    let layer = d.default_layer().unwrap();
    for vertical in [true, false] {
        let mut path = PathData::default();
        for i in 0..200 {
            let rect = if vertical {
                Rect::new(i as f64 * 2., 0., i as f64 * 2. + 1., 400.)
            } else {
                Rect::new(0., i as f64 * 2., 400., i as f64 * 2. + 1.)
            };
            path.subpaths.extend(shapes::rectangle(rect).subpaths);
        }
        let n = Node::path(
            d.alloc_id(),
            path,
            Appearance::basic(Paint::solid(if vertical { Color::BLACK } else { Color::rgb(1., 0., 0.) }), Paint::None, 0.),
        );
        d.insert(Some(layer), usize::MAX, n).unwrap();
    }
    let mut nearest = vec![Some(0)];
    assert!(colour::samples(&d, &mut nearest, 1, Rect::new(0., 0., 1., 1.), 0.7).unwrap().is_none());
    assert_eq!(nearest, [Some(0)], "renderer samples survive a complexity fallback");
    let token = crate::cancel::CancelToken::new();
    token.cancel();
    let result = crate::cancel::scope(&token, None, || colour::samples(&d, &mut nearest, 1, Rect::new(0., 0., 1., 1.), 0.7));
    assert!(matches!(result, Err(crate::EngineError::Cancelled)));
}

#[test]
fn band_halo_matches_whole_grid_across_seams_and_holes() {
    let s = session(Color::cmyk(1., 0., 0., 0.));
    let source = &s.doc().unwrap().doc;
    let mut eligible = (**source).clone();
    for layer in &mut eligible.layers {
        eligibility(Arc::make_mut(layer), source, 0.7, true, &mut Vec::new()).unwrap();
    }
    let region = Rect::new(0., 0., 30., 30.);
    let mut renderer = Renderer::new();
    let whole = render_band(source, &eligible, None, &mut renderer, region, 250, 250, 0.7, 0, 250).unwrap();
    for (start, end) in [(0usize, 47usize), (47, 100), (100, 203), (203, 250)] {
        let top = start.saturating_sub(8usize);
        let bottom = (end + 8).min(250);
        let r = Rect::new(0., top as f64 / SCALE, 30., bottom as f64 / SCALE);
        let part = render_band(source, &eligible, None, &mut renderer, r, 250, (bottom - top) as u32, 0.7, start - top, end - top).unwrap();
        assert_eq!(&part.mask[(start - top) * 250..(end - top) * 250], &whole.mask[start * 250..end * 250]);
        assert_eq!(&part.process[(start - top) * 250 * 4..(end - top) * 250 * 4], &whole.process[start * 250 * 4..end * 250 * 4]);
    }
}

#[test]
fn a2_grid_is_supported_but_pathological_dimensions_are_bounded() {
    let a2 = Rect::new(0., 0., 420. * MM, 594. * MM).inflate(0.3 * MM, 0.3 * MM);
    let (w, h) = vectorcraft_render::raster_size(a2, SCALE).unwrap();
    assert!(w as usize * h as usize <= MAX_PIXELS);
    let mut rows = 0;
    for b in bands::rows(w, h, a2, 8) {
        assert!(w as usize * b.height as usize <= BAND_PIXELS + w as usize * 16);
        rows += b.last - b.first;
    }
    assert_eq!(rows, h as usize);
    assert!(pixels::band(&[], &[], &[], 15001, 10000, 4.).is_err());
}

#[test]
fn rasterize_keeps_process_plates_in_one_image() {
    let mut s = session(Color::cmyk(1., 0.2, 0., 0.));
    let id = s.doc().unwrap().doc.layers[0].children().unwrap()[0].id;
    s.edit("test", |d, _| {
        let node = d.node_mut(id).unwrap();
        if let NodeKind::Path { path, .. } = &mut node.kind {
            *path = shapes::rectangle(Rect::new(0., 0., 120., 600.));
        }
        Ok(())
    })
    .unwrap();
    let before = s.doc().unwrap().doc.clone();
    raster::rasterize_with_limit(&mut s, &[id], 0).unwrap();
    let d = &s.doc().unwrap().doc;
    assert_eq!(d.images.len(), 2, "one ink image and its mask, without RIP interpolation seams");
    let mut renderer = Renderer::new();
    // Check the ink samples around an internal scanline, including all process plates.
    let region = Rect::new(0., 479., 120., 481.);
    for plate in ["Cyan", "Magenta", "Yellow", "Black"] {
        let a = renderer.render_region_with(&before, region, SCALE, &options(Some(plate)));
        let b = renderer.render_region_with(d, region, SCALE, &options(Some(plate)));
        assert_eq!(a.pixels, b.pixels, "{plate}");
    }
}

#[test]
fn contour_budget_bounds_islands_holes_and_single_noisy_contours_before_tracing() {
    let w = 1024;
    let noisy: Vec<_> = (0..w * w).map(|i| (i % w + i / w) % 2 == 0).collect();
    assert!(check_contour_budget(&noisy, w, w, MAX_CONTOUR_ANCHORS).unwrap_err().to_string().contains("contour/anchor memory budget"));
    // One connected region with a jagged edge, then many holes in one region.
    for mask in [(0..256).map(|i| i / 16 == 0 || i % 2 == 0).collect::<Vec<_>>(), (0..256).map(|i| i / 16 % 2 == 0 || i % 2 == 0).collect()] {
        assert!(check_contour_budget(&mask, 16, 16, 20).is_err());
    }
    // Large smooth contours consume few anchors regardless of their perimeter.
    check_contour_budget(&vec![true; w * w], w, w, 5).unwrap();
    assert!(check_contour_budget(&[true], 1, 1, 4).is_err());
    check_contour_budget(&[true], 1, 1, 5).unwrap();
    // Exhaustively verify the bound includes the tracer's extra starting vertices.
    for bits in 0u32..1 << 16 {
        let mask: Vec<_> = (0..16).map(|i| bits >> i & 1 == 1).collect();
        let components = vectorcraft_trace::trace_mask(&mask, 4, 4);
        let anchors: usize = components.iter().map(|c| c.outer.pts.len() + c.holes.iter().map(|h| h.pts.len()).sum::<usize>()).sum();
        if anchors > 0 {
            assert!(check_contour_budget(&mask, 4, 4, anchors - 1).is_err(), "{bits:016b}");
        }
        check_contour_budget(&mask, 4, 4, 80).unwrap();
    }
}

#[test]
fn banded_rasterize_contour_stop_preserves_art_and_selection() {
    let mut s = session(Color::BLACK);
    let id = s.doc().unwrap().doc.layers[0].children().unwrap()[0].id;
    let rgba = image::RgbaImage::from_fn(32, 32, |x, y| image::Rgba([255, 0, 0, if (x + y) % 2 == 0 { 255 } else { 0 }]));
    let mut png = std::io::Cursor::new(Vec::new());
    rgba.write_to(&mut png, image::ImageFormat::Png).unwrap();
    s.edit("test", |d, sel| {
        d.images.insert("noise".into(), ImageBlob::png(png.into_inner()));
        let image = image_node(d, "noise".into(), 32, 32, Affine::scale(1. / SCALE));
        d.node_mut(id).unwrap().kind = image.kind;
        sel.set([id]);
        Ok(())
    })
    .unwrap();
    let before = s.doc().unwrap().doc.clone();
    let selected = s.doc().unwrap().selection.objects.clone();
    let err = raster::rasterize_with_budgets(&mut s, &[id], 0, 64).unwrap_err();
    assert!(err.to_string().contains("contour/anchor memory budget"), "{err}");
    assert_eq!(s.doc().unwrap().doc, before);
    assert_eq!(s.doc().unwrap().selection.objects, selected);
}
