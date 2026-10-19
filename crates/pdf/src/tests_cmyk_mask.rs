use super::*;
use vectorcraft_color::BlendMode;
use vectorcraft_doc::Knockout;

fn fixture(alpha: &[u8]) -> Vec<u8> {
    let mask = stream("/Type /XObject /Subtype /Image /Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", alpha);
    let inks = vec![0, 32, 64, 96, 128, 160, 192, 224, 255, 0, 128, 0, 40, 80, 120, 160, 17, 34, 51, 68, 0, 255, 0, 255];
    pdf_with_images(&[(cmyk_dict(3, 2, "/SMask 6 0 R"), inks)], &[mask])
}

fn image_node(d: &Document) -> &Node {
    d.layers[0].children().unwrap().iter().find(|n| matches!(n.kind, NodeKind::Image(_))).unwrap()
}

fn image_node_mut(d: &mut Document) -> &mut Node {
    let layer = Arc::make_mut(&mut d.layers[0]);
    Arc::make_mut(layer.children_mut().unwrap().iter_mut().find(|n| matches!(n.kind, NodeKind::Image(_))).unwrap())
}

fn hard() -> PdfOptions {
    opts(json!({"disableImageInterpolation": true}))
}

#[test]
fn exact_cmyk_mask_keeps_every_ink_and_alpha_through_two_saves() {
    for (name, alpha) in [("binary", [0, 255, 255, 0, 255, 255]), ("fractional", [0, 51, 102, 153, 204, 255])] {
        let input = fixture(&alpha);
        let source = print_order(&input).unwrap();
        let expected = source.images[0][0].samples.as_ref().unwrap();
        let mut doc = import(&input).unwrap();
        for round in 1..=2 {
            let before = doc.clone();
            let bytes = export(&doc, &hard()).unwrap();
            assert_eq!(doc, before, "export must not consume editable mask art");
            let raw = print_order(&bytes).unwrap();
            assert_eq!(raw.images[0].len(), 1);
            let use_ = &raw.images[0][0];
            assert!(use_.description.soft_mask, "{name} round {round}");
            assert!(!use_.compositing_unresolved, "{use_:?}");
            assert_eq!(use_.description.colour_space, "DeviceCMYK");
            let actual = use_.samples.as_ref().unwrap();
            assert_eq!((actual.width, actual.height), (3, 2));
            for pixel in 0..6 {
                for ink in 0..4 {
                    assert_eq!(actual.tone(pixel, ink), expected.tone(pixel, ink));
                }
                assert_eq!(actual.alpha(pixel), expected.alpha(pixel));
            }
            if let Some(root) = std::env::var_os("CHECK_CMYK_MASK_EVIDENCE_DIR") {
                let root = std::path::PathBuf::from(root);
                std::fs::create_dir_all(&root).unwrap();
                std::fs::write(root.join(format!("{name}-original.pdf")), &input).unwrap();
                std::fs::write(root.join(format!("{name}-save-{round}.pdf")), &bytes).unwrap();
            }
            doc = import(&bytes).unwrap();
        }
    }
}

#[test]
fn complex_cmyk_mask_art_does_not_enter_the_exact_pair_path() {
    type Change = (&'static str, fn(&mut Node));
    let cases: &[Change] = &[
        ("disabled", |n| n.extras_mut().mask.as_mut().unwrap().disabled = true),
        ("unclipped", |n| n.extras_mut().mask.as_mut().unwrap().clip = false),
        ("inverted", |n| n.extras_mut().mask.as_mut().unwrap().invert = true),
        ("source opacity", |n| n.opacity = 0.5),
        ("source blend", |n| n.blend = BlendMode::Multiply),
        ("source isolation", |n| n.isolate = true),
        ("source knockout", |n| n.knockout = Knockout::On),
        ("source knockout shape", |n| n.knockout_shape = true),
        ("mask opacity", |n| Arc::make_mut(&mut n.extras_mut().mask.as_mut().unwrap().art).opacity = 0.5),
        ("mask blend", |n| Arc::make_mut(&mut n.extras_mut().mask.as_mut().unwrap().art).blend = BlendMode::Multiply),
        ("mask dimensions", |n| {
            let mut e = n.extras_mut();
            let a = Arc::make_mut(&mut e.mask.as_mut().unwrap().art);
            if let NodeKind::Image(im) = &mut a.kind {
                im.width = 2;
            }
        }),
        ("mask placement", |n| {
            let mut e = n.extras_mut();
            let a = Arc::make_mut(&mut e.mask.as_mut().unwrap().art);
            if let NodeKind::Image(im) = &mut a.kind {
                im.xf = Affine::translate((0.001, 0.0)) * im.xf;
            }
        }),
        ("degenerate placement", |n| {
            if let NodeKind::Image(im) = &mut n.kind {
                im.xf = Affine::scale(0.0);
            }
        }),
        ("nonfinite placement", |n| {
            if let NodeKind::Image(im) = &mut n.kind {
                im.xf = Affine::scale(f64::NAN);
            }
        }),
    ];
    for (name, change) in cases {
        let mut doc = import(&fixture(&[0, 51, 102, 153, 204, 255])).unwrap();
        assert!(crate::images::masked_cmyk(&doc, image_node(&doc)).is_some());
        change(image_node_mut(&mut doc));
        assert!(crate::images::masked_cmyk(&doc, image_node(&doc)).is_none(), "{name}");
    }
}

#[test]
fn compression_conversion_and_enclosing_compositing_keep_group_masks() {
    let doc = import(&fixture(&[0, 51, 102, 153, 204, 255])).unwrap();
    for settings in [
        json!({}),
        json!({"disableImageInterpolation":true,"compression":{"color":{"downsample":"bicubic"}}}),
        json!({"disableImageInterpolation":true,"compression":{"gray":{"compression":"jpeg"}}}),
        json!({"disableImageInterpolation":true,"compression":{"color":{"compression":"jpeg"}}}),
        json!({"disableImageInterpolation":true,"output":{"conversion":"destination","destination":DEVICE_CMYK}}),
        json!({"disableImageInterpolation":true,"output":{"profiles":"all"}}),
        json!({"disableImageInterpolation":true,"standard":"pdfX4"}),
    ] {
        let raw = print_order(&export(&doc, &opts(settings.clone())).unwrap()).unwrap();
        assert!(!raw.images[0][0].description.soft_mask, "{settings}");
        assert!(raw.images[0][0].compositing_unresolved, "{settings}");
    }
    for mode in 0..5 {
        let mut d = doc.clone();
        let outer_mask = image_node(&d).extras().mask.clone();
        let parent = Arc::make_mut(&mut d.layers[0]);
        match mode {
            0 => parent.opacity = 0.5,
            1 => parent.blend = BlendMode::Multiply,
            2 => parent.isolate = true,
            3 => parent.knockout = Knockout::On,
            _ => parent.extras_mut().mask = outer_mask,
        }
        let raw = print_order(&export(&d, &hard()).unwrap()).unwrap();
        assert!(!raw.images[0][0].description.soft_mask, "parent mode {mode}");
        assert!(raw.images[0][0].compositing_unresolved, "parent mode {mode}");
    }
}

#[test]
fn cmyk_mask_requires_opaque_gray_bytes_and_matching_decoded_grid() {
    for mode in 0..3 {
        let mut doc = import(&fixture(&[0, 51, 102, 153, 204, 255])).unwrap();
        let mask = image_node(&doc).extras().mask.as_ref().unwrap();
        let NodeKind::Image(mi) = &mask.art.kind else { panic!() };
        let key = mi.key.clone();
        let image = match mode {
            0 => image::DynamicImage::ImageLuma8(image::GrayImage::new(2, 2)),
            1 => image::DynamicImage::ImageLumaA8(image::GrayAlphaImage::new(3, 2)),
            _ => image::DynamicImage::ImageRgb8(image::RgbImage::new(3, 2)),
        };
        let mut bytes = Vec::new();
        image.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        doc.images.insert(key, ImageBlob::new("image/png", bytes));
        assert!(crate::images::masked_cmyk(&doc, image_node(&doc)).is_none());
    }
}

#[test]
fn distinct_masks_do_not_alias_and_complex_siblings_restore_export_context() {
    let mut doc = import(&fixture(&[0, 51, 102, 153, 204, 255])).unwrap();
    let first = image_node(&doc).clone();
    let mut other = first.clone();
    other.id = vectorcraft_doc::NodeId(992);
    let mut alpha = Vec::new();
    image::GrayImage::from_raw(3, 2, vec![255, 204, 153, 102, 51, 0])
        .unwrap()
        .write_to(&mut std::io::Cursor::new(&mut alpha), image::ImageFormat::Png)
        .unwrap();
    let blob = ImageBlob::new("image/png", alpha);
    let key = blob.content_key();
    doc.images.insert(key.clone(), blob);
    {
        let mut e = other.extras_mut();
        let art = Arc::make_mut(&mut e.mask.as_mut().unwrap().art);
        let NodeKind::Image(im) = &mut art.kind else { panic!() };
        im.key = key;
    }
    other.transform(Affine::translate((60.0, 0.0)), false);
    let mut in_group = first.clone();
    in_group.id = vectorcraft_doc::NodeId(993);
    let mut group = Node::group(vectorcraft_doc::NodeId(991), vec![Arc::new(in_group)]);
    group.opacity = 0.5;
    *Arc::make_mut(&mut doc.layers[0]).children_mut().unwrap() = vec![Arc::new(group), Arc::new(first), Arc::new(other)];
    let raw = print_order(&export(&doc, &hard()).unwrap()).unwrap();
    assert_eq!(raw.images[0].len(), 3);
    assert!(!raw.images[0][0].description.soft_mask);
    for (i, expected) in [(1, [0, 51, 102, 153, 204, 255]), (2, [255, 204, 153, 102, 51, 0])] {
        let image = &raw.images[0][i];
        assert!(image.description.soft_mask);
        let samples = image.samples.as_ref().unwrap();
        for (pixel, alpha) in expected.iter().enumerate() {
            assert!((samples.alpha(pixel).unwrap() - f64::from(*alpha) / 255.0).abs() < 1e-12);
        }
    }
}

#[test]
fn declared_mask_profile_and_original_jpeg_keep_existing_export_policy() {
    use image::ImageEncoder;
    let mut doc = import(&fixture(&[0, 51, 102, 153, 204, 255])).unwrap();
    let mask = image_node(&doc).extras().mask.as_ref().unwrap();
    let NodeKind::Image(mi) = &mask.art.kind else { panic!() };
    let key = mi.key.clone();
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    // A declared profile is refused even if the writer would later reject its contents.
    encoder.set_icc_profile(vec![0; 128]).unwrap();
    encoder.write_image(&[0, 51, 102, 153, 204, 255], 3, 2, image::ExtendedColorType::L8).unwrap();
    doc.images.insert(key, ImageBlob::new("image/png", bytes));
    assert!(crate::images::masked_cmyk(&doc, image_node(&doc)).is_none());
    let mut doc = import(&fixture(&[0, 51, 102, 153, 204, 255])).unwrap();
    let NodeKind::Image(im) = &image_node(&doc).kind else { panic!() };
    let key = im.key.clone();
    doc.images.insert(key, ImageBlob::new("image/jpeg", cmyk_jpeg(&flat([0, 102, 255, 0], 6), 3, 2)));
    let raw = print_order(&export(&doc, &hard()).unwrap()).unwrap();
    assert!(!raw.images[0][0].description.soft_mask);
    assert!(raw.images[0][0].compositing_unresolved);
}
