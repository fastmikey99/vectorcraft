//! Remove Soft Pixels (`object.image.solidifyAlpha`).

use std::io::Cursor;
use std::sync::Arc;

use image::DynamicImage;
use serde_json::{Value, json};
use vectorcraft_doc::{Document, ImageBlob, ImageObject, LinkInfo, Node, NodeKind, OpacityMask};
use vectorcraft_geom::Affine;

use super::*;

const C: &str = "object.image.solidifyAlpha";

/// The alphas of the test row: clear, the softest, either side of the default level, soft, the
/// most opaque soft value, full.
const ALPHAS: [u8; 7] = [0, 1, 127, 128, 200, 254, 255];

fn png(img: DynamicImage) -> Vec<u8> {
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

/// A row of RGBA pixels, one per alpha of [`ALPHAS`], each a different colour.
fn rgba_row() -> image::RgbaImage {
    let px: Vec<u8> = ALPHAS.iter().enumerate().flat_map(|(i, a)| [10 + i as u8, 200 - i as u8 * 7, 33 * i as u8, *a]).collect();
    image::RgbaImage::from_raw(ALPHAS.len() as u32, 1, px).unwrap()
}

/// An image object showing blob `key` (`w` × `h` pixels) scaled by `scale` at (`x`, 0).
fn image_node(d: &mut Document, key: &str, (w, h): (u32, u32), x: f64, scale: f64) -> Node {
    let xf = Affine::translate((x, 0.0)) * Affine::scale(scale);
    Node::new(
        d.alloc_id(),
        NodeKind::Image(Box::new(ImageObject { key: key.into(), width: w, height: h, xf, link: None, placement: Default::default() })),
    )
}

/// A session over a document holding blob `bytes` as `key`, shown by images at `scales` (in that
/// order, side by side) → the session and the images' ids.
fn session_with(bytes: Vec<u8>, size: (u32, u32), scales: &[f64]) -> (Session, Vec<NodeId>) {
    let mut d = Document::new(400.0, 400.0);
    d.images.insert("src".into(), ImageBlob::new("image/png", bytes));
    let layer = d.layers[0].id;
    let mut ids = vec![];
    for (i, sc) in scales.iter().enumerate() {
        let n = image_node(&mut d, "src", size, i as f64 * 50.0, *sc);
        ids.push(n.id);
        d.insert(Some(layer), i, n).unwrap();
    }
    let mut s = Session::new();
    s.add_document(d, None);
    (s, ids)
}

fn select(s: &mut Session, id: NodeId) {
    s.execute("select.set", &json!({"ids": [id.0]})).unwrap();
}

fn key_of(s: &Session, id: NodeId) -> String {
    match &s.doc().unwrap().doc.node(id).unwrap().kind {
        NodeKind::Image(im) => im.key.clone(),
        _ => panic!("not an image"),
    }
}

fn blob(s: &Session, key: &str) -> ImageBlob {
    s.doc().unwrap().doc.images.get(key).unwrap().clone()
}

fn decoded(b: &ImageBlob) -> DynamicImage {
    image::load_from_memory(&b.bytes).unwrap()
}

fn counts(r: &Value) -> (u64, u64, u64) {
    (r["softPixels"].as_u64().unwrap(), r["clearPixels"].as_u64().unwrap(), r["opaquePixels"].as_u64().unwrap())
}

#[test]
fn an_rgba_image_becomes_opaque_or_clear_with_its_colours_kept_in_one_undo_step() {
    let src = rgba_row();
    let (mut s, ids) = session_with(png(DynamicImage::ImageRgba8(src.clone())), (7, 1), &[1.0]);
    select(&mut s, ids[0]);
    let before = s.doc().unwrap().doc.clone();
    let undo = s.doc().unwrap().history.undo.len();
    let r = s.execute(C, &json!({})).unwrap();
    assert_eq!(r["form"], "alpha");
    assert_eq!(r["ids"], json!([ids[0].0]));
    assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["level"].as_u64()), (Some(7), Some(1), Some(128)));
    assert_eq!(counts(&r), (5, 3, 4));
    assert_eq!(s.doc().unwrap().history.undo.len(), undo + 1, "one undo step");
    let key = key_of(&s, ids[0]);
    assert_ne!(key, "src", "the result goes in under its own key");
    let out = decoded(&blob(&s, &key));
    let DynamicImage::ImageRgba8(out) = out else { panic!("RGBA stays RGBA, 8 bits") };
    for (o, i) in out.pixels().zip(src.pixels()) {
        assert_eq!(o.0[..3], i.0[..3], "colour samples bit for bit, soft pixels too");
        assert_eq!(o.0[3], if i.0[3] >= 128 { 255 } else { 0 });
    }
    // The source blob is still there, unchanged.
    assert_eq!(blob(&s, "src"), before.images["src"]);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(*s.doc().unwrap().doc, *before, "undo restores the original bytes and key");
    assert_eq!(key_of(&s, ids[0]), "src");
}

#[test]
fn the_level_is_the_alpha_that_becomes_opaque() {
    let (mut s, ids) = session_with(png(DynamicImage::ImageRgba8(rgba_row())), (7, 1), &[1.0]);
    select(&mut s, ids[0]);
    let r = s.execute(C, &json!({"level": 255})).unwrap();
    assert_eq!(counts(&r), (5, 6, 1));
    s.execute("edit.undo", &json!({})).unwrap();
    let r = s.execute(C, &json!({"level": 1})).unwrap();
    assert_eq!(counts(&r), (5, 1, 6));
    for bad in [json!(0), json!(256), json!(12.5), json!("128"), json!(-1)] {
        let e = s.execute(C, &json!({"level": bad})).unwrap_err().to_string();
        assert!(e.contains("level must be a whole number from 1 to 255"), "{bad}: {e}");
    }
    assert!(s.execute(C, &json!({"copies": "yes"})).unwrap_err().to_string().contains("copies must be true or false"));
}

#[test]
fn sixteen_bit_and_grey_alpha_keep_their_colour_type() {
    // 16 bits: the cut is level × 257.
    let px: Vec<u16> =
        [0u16, 1, 128 * 257 - 1, 128 * 257, 65534, 65535].iter().enumerate().flat_map(|(i, a)| [i as u16 * 1001, 7, 65535 - i as u16, *a]).collect();
    let src = image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(6, 1, px).unwrap();
    let (mut s, ids) = session_with(png(DynamicImage::ImageRgba16(src.clone())), (6, 1), &[1.0]);
    select(&mut s, ids[0]);
    let r = s.execute(C, &json!({})).unwrap();
    assert_eq!(counts(&r), (4, 3, 3));
    let DynamicImage::ImageRgba16(out) = decoded(&blob(&s, &key_of(&s, ids[0]))) else { panic!("16-bit RGBA stays so") };
    for (o, i) in out.pixels().zip(src.pixels()) {
        assert_eq!(o.0[..3], i.0[..3]);
        assert_eq!(o.0[3], if i.0[3] >= 128 * 257 { 65535 } else { 0 });
    }
    // Grey and alpha.
    let src = image::GrayAlphaImage::from_raw(7, 1, ALPHAS.iter().enumerate().flat_map(|(i, a)| [i as u8 * 30, *a]).collect()).unwrap();
    let (mut s, ids) = session_with(png(DynamicImage::ImageLumaA8(src.clone())), (7, 1), &[1.0]);
    select(&mut s, ids[0]);
    assert_eq!(counts(&s.execute(C, &json!({})).unwrap()), (5, 3, 4));
    let DynamicImage::ImageLumaA8(out) = decoded(&blob(&s, &key_of(&s, ids[0]))) else { panic!("grey + alpha stays so") };
    assert!(out.pixels().zip(src.pixels()).all(|(o, i)| o.0[0] == i.0[0] && o.0[1] == if i.0[1] >= 128 { 255 } else { 0 }));
}

#[test]
fn a_png_keeps_its_resolution_and_colour_chunks() {
    // A pHYs chunk (300 ppi: 11811 pixels a metre) and a gAMA chunk after the header.
    let chunk = |kind: &[u8; 4], data: &[u8]| {
        let mut c = (data.len() as u32).to_be_bytes().to_vec();
        c.extend_from_slice(kind);
        c.extend_from_slice(data);
        let mut crc = kind.to_vec();
        crc.extend_from_slice(data);
        c.extend_from_slice(&crc32(&crc).to_be_bytes());
        c
    };
    let plain = png(DynamicImage::ImageRgba8(rgba_row()));
    let mut with = plain[..33].to_vec();
    with.extend(chunk(b"pHYs", &[0, 0, 0x2e, 0x23, 0, 0, 0x2e, 0x23, 1]));
    with.extend(chunk(b"gAMA", &45455u32.to_be_bytes()));
    with.extend_from_slice(&plain[33..]);
    assert!(image::load_from_memory(&with).is_ok());
    let (mut s, ids) = session_with(with, (7, 1), &[1.0]);
    select(&mut s, ids[0]);
    s.execute(C, &json!({})).unwrap();
    let out = blob(&s, &key_of(&s, ids[0])).bytes;
    let has = |kind: &[u8]| out.windows(4).any(|w| w == kind);
    assert!(has(b"pHYs") && has(b"gAMA"), "the source's chunks are kept");
    assert!(decoded(&blob(&s, &key_of(&s, ids[0]))).as_rgba8().is_some());
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

#[test]
fn an_image_without_alpha_has_nothing_to_remove() {
    let rgb = png(DynamicImage::ImageRgb8(image::RgbImage::from_pixel(3, 3, image::Rgb([1, 2, 3]))));
    let mut jpeg = Vec::new();
    DynamicImage::ImageRgb8(image::RgbImage::from_pixel(3, 3, image::Rgb([1, 2, 3])))
        .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .unwrap();
    let cmyk = ImageBlob::cmyk_tiff(&vectorcraft_doc::cmyk::Inks::new(3, 3, vec![40; 36]).unwrap()).unwrap();
    for bytes in [rgb, jpeg, Arc::unwrap_or_clone(cmyk.bytes)] {
        let (mut s, ids) = session_with(bytes, (3, 3), &[1.0]);
        select(&mut s, ids[0]);
        let undo = s.doc().unwrap().history.undo.len();
        let e = s.execute(C, &json!({})).unwrap_err().to_string();
        assert!(e.contains("the image has no transparency: nothing to remove"), "{e}");
        assert_eq!(s.doc().unwrap().history.undo.len(), undo, "nothing changed");
    }
}

#[test]
fn an_image_already_solid_changes_nothing() {
    let px: Vec<u8> = [0u8, 255, 255, 0].iter().flat_map(|a| [5, 6, 7, *a]).collect();
    let (mut s, ids) = session_with(png(DynamicImage::ImageRgba8(image::RgbaImage::from_raw(4, 1, px).unwrap())), (4, 1), &[1.0]);
    select(&mut s, ids[0]);
    let undo = s.doc().unwrap().history.undo.len();
    let r = s.execute(C, &json!({})).unwrap();
    assert_eq!((r["ids"].clone(), counts(&r)), (json!([]), (0, 2, 2)));
    assert_eq!(s.doc().unwrap().history.undo.len(), undo, "no undo step");
    assert_eq!(key_of(&s, ids[0]), "src");
}

#[test]
fn only_one_embedded_image_is_taken() {
    let (mut s, ids) = session_with(png(DynamicImage::ImageRgba8(rgba_row())), (7, 1), &[1.0, 1.0]);
    s.execute("select.set", &json!({"ids": [ids[0].0, ids[1].0]})).unwrap();
    assert!(s.execute(C, &json!({})).unwrap_err().to_string().contains("select one image to remove its soft pixels"));
    s.execute("select.none", &json!({})).unwrap();
    assert!(s.execute(C, &json!({})).unwrap_err().to_string().contains("select one image"));
    // By id, without a selection.
    assert_eq!(s.execute(C, &json!({"id": ids[1].0})).unwrap()["ids"], json!([ids[1].0]));
    let r = s.execute("shape.rectangle", &json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap();
    assert!(s.execute(C, &json!({"id": r["id"]})).unwrap_err().to_string().contains("id must name an image"));
    assert!(s.execute(C, &json!({"id": 999_999})).is_err());
    // A linked image.
    let id = ids[0];
    s.edit("t", |d, _| {
        if let NodeKind::Image(im) = &mut d.node_mut(id).unwrap().kind {
            im.link = Some(LinkInfo { path: "/tmp/x.png".into(), ..Default::default() });
        }
        Ok(())
    })
    .unwrap();
    assert!(s.execute(C, &json!({"id": id.0})).unwrap_err().to_string().contains("the image is linked: embed it to remove its soft pixels"));
}

#[test]
fn copies_at_the_same_printed_size_change_together_or_not() {
    let png = png(DynamicImage::ImageRgba8(rgba_row()));
    // Two at the same size, one at twice it.
    for all in [false, true] {
        let (mut s, ids) = session_with(png.clone(), (7, 1), &[1.0, 1.0, 2.0]);
        select(&mut s, ids[1]);
        let undo = s.doc().unwrap().history.undo.len();
        let before = s.doc().unwrap().doc.clone();
        let r = s.execute(C, &json!({"copies": all})).unwrap();
        let changed: Vec<u64> = if all { vec![ids[0].0, ids[1].0] } else { vec![ids[1].0] };
        assert_eq!(r["ids"], json!(changed), "copies {all}");
        assert_eq!(s.doc().unwrap().history.undo.len(), undo + 1, "one undo step");
        let new = key_of(&s, ids[1]);
        assert_eq!(key_of(&s, ids[0]) == new, all);
        assert_eq!(key_of(&s, ids[2]), "src", "the copy at another size keeps its pixels");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(*s.doc().unwrap().doc, *before);
    }
}

/// A document with a CMYK image (`inks`) masked by greyscale image `mask` (`w` × `h`), `copies` times.
fn masked_session(mask: Vec<u8>, w: u32, h: u32, invert: bool, copies: usize) -> (Session, Vec<NodeId>, Vec<u8>) {
    let inks: Vec<u8> = (0..w * h * 4).map(|i| (i * 37 % 251) as u8).collect();
    let cmyk = ImageBlob::cmyk_tiff(&vectorcraft_doc::cmyk::Inks::new(w, h, inks).unwrap()).unwrap();
    let colour = cmyk.bytes.to_vec();
    let mut d = Document::new(400.0, 400.0);
    d.images.insert("ink".into(), cmyk);
    d.images.insert("mask".into(), ImageBlob::new("image/png", png(DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, mask).unwrap()))));
    let layer = d.layers[0].id;
    let mut ids = vec![];
    for i in 0..copies {
        let x = i as f64 * 50.0;
        let art = image_node(&mut d, "mask", (w, h), x, 1.0);
        let mut m = OpacityMask::new(art, true);
        m.invert = invert;
        let n = image_node(&mut d, "ink", (w, h), x, 1.0).with_mask(Some(Box::new(m)));
        ids.push(n.id);
        d.insert(Some(layer), i, n).unwrap();
    }
    let mut s = Session::new();
    s.add_document(d, None);
    (s, ids, colour)
}

fn mask_key(s: &Session, id: NodeId) -> String {
    match &s.doc().unwrap().doc.node(id).unwrap().extras().mask.as_ref().unwrap().art.kind {
        NodeKind::Image(im) => im.key.clone(),
        _ => panic!("mask isn't an image"),
    }
}

#[test]
fn a_cmyk_image_with_a_soft_mask_thresholds_the_mask_and_keeps_its_inks() {
    let (mut s, ids, colour) = masked_session(ALPHAS.to_vec(), 7, 1, false, 2);
    select(&mut s, ids[0]);
    let before = s.doc().unwrap().doc.clone();
    let r = s.execute(C, &json!({"copies": true})).unwrap();
    assert_eq!(r["form"], "mask");
    assert_eq!(r["ids"], json!([ids[0].0, ids[1].0]));
    assert_eq!(counts(&r), (5, 3, 4));
    for id in &ids {
        assert_eq!(key_of(&s, *id), "ink", "the colour image isn't touched");
        let k = mask_key(&s, *id);
        assert_ne!(k, "mask");
        let DynamicImage::ImageLuma8(m) = decoded(&blob(&s, &k)) else { panic!("the mask stays greyscale") };
        assert_eq!(m.as_raw(), &ALPHAS.map(|a| if a >= 128 { 255 } else { 0 }).to_vec());
    }
    assert_eq!(*blob(&s, "ink").bytes, colour, "the ink samples are bit for bit what they were");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(*s.doc().unwrap().doc, *before);
}

#[test]
fn an_inverted_mask_is_thresholded_as_the_alpha_it_gives() {
    // Inverted: the alpha is 255 − the mask's level.
    let (mut s, ids, _) = masked_session(ALPHAS.map(|a| 255 - a).to_vec(), 7, 1, true, 1);
    select(&mut s, ids[0]);
    assert_eq!(counts(&s.execute(C, &json!({})).unwrap()), (5, 3, 4));
    let DynamicImage::ImageLuma8(m) = decoded(&blob(&s, &mask_key(&s, ids[0]))) else { panic!() };
    assert_eq!(m.as_raw(), &ALPHAS.map(|a| if a >= 128 { 0 } else { 255 }).to_vec());
}

#[test]
fn a_mask_that_isnt_one_greyscale_image_or_over_alpha_is_refused() {
    // The mask's image has colour.
    let (mut s, ids, _) = masked_session(ALPHAS.to_vec(), 7, 1, false, 1);
    let id = ids[0];
    let rgb = png(DynamicImage::ImageRgb8(image::RgbImage::from_pixel(7, 1, image::Rgb([9, 9, 9]))));
    s.edit("t", |d, _| {
        d.images.insert("mask".into(), ImageBlob::new("image/png", rgb));
        Ok(())
    })
    .unwrap();
    select(&mut s, id);
    assert!(s.execute(C, &json!({})).unwrap_err().to_string().contains("the image's opacity mask isn't one greyscale image"));
    // The mask is vector art.
    s.edit("t", |d, _| {
        let art = Node::path(d.alloc_id(), vectorcraft_geom::shapes::rectangle(vectorcraft_geom::Rect::new(0.0, 0.0, 5.0, 5.0)), Default::default());
        d.node_mut(id).unwrap().extras_mut().mask = Some(Box::new(OpacityMask::new(art, true)));
        Ok(())
    })
    .unwrap();
    assert!(s.execute(C, &json!({})).unwrap_err().to_string().contains("the image's opacity mask isn't one greyscale image"));
    // An RGBA image under a greyscale mask: two transparencies.
    let (mut s, ids) = session_with(png(DynamicImage::ImageRgba8(rgba_row())), (7, 1), &[1.0]);
    let id = ids[0];
    s.edit("t", |d, _| {
        d.images
            .insert("mask".into(), ImageBlob::new("image/png", png(DynamicImage::ImageLuma8(image::GrayImage::from_pixel(7, 1, image::Luma([90]))))));
        let art = image_node(d, "mask", (7, 1), 0.0, 1.0);
        d.node_mut(id).unwrap().extras_mut().mask = Some(Box::new(OpacityMask::new(art, true)));
        Ok(())
    })
    .unwrap();
    select(&mut s, id);
    assert!(s.execute(C, &json!({})).unwrap_err().to_string().contains("release the mask first"));
    // A disabled mask is not in force: the image's own alpha is thresholded.
    s.edit("t", |d, _| {
        d.node_mut(id).unwrap().extras_mut().mask.as_deref_mut().unwrap().disabled = true;
        Ok(())
    })
    .unwrap();
    assert_eq!(s.execute(C, &json!({})).unwrap()["form"], "alpha");
}

#[test]
fn the_check_catches_a_result_that_isnt_the_threshold() {
    let src = DynamicImage::ImageRgba8(rgba_row());
    let (good, _) = crate::cmd::solidify::solidified(&src, 128, None).unwrap();
    assert!(crate::cmd::solidify::check(&src, &good, 128, None));
    assert!(!crate::cmd::solidify::check(&src, &good, 129, None), "another level");
    let mut colour = good.to_rgba8();
    colour.get_pixel_mut(2, 0).0[0] ^= 1;
    assert!(!crate::cmd::solidify::check(&src, &DynamicImage::ImageRgba8(colour), 128, None), "a colour sample changed");
    let mut soft = good.to_rgba8();
    soft.get_pixel_mut(4, 0).0[3] = 200;
    assert!(!crate::cmd::solidify::check(&src, &DynamicImage::ImageRgba8(soft), 128, None), "a soft alpha left");
    assert!(!crate::cmd::solidify::check(&src, &DynamicImage::ImageRgba16(good.to_rgba16()), 128, None), "another colour type");
}

#[test]
fn an_id_given_must_name_an_image_and_never_falls_back_to_the_selection() {
    let (mut s, ids) = session_with(png(DynamicImage::ImageRgba8(rgba_row())), (7, 1), &[1.0]);
    select(&mut s, ids[0]);
    let rect = s.execute("shape.rectangle", &json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap()["id"].clone();
    select(&mut s, ids[0]);
    let before = s.doc().unwrap().doc.clone();
    let undo = s.doc().unwrap().history.undo.len();
    for bad in [json!("invalid"), json!(-1), json!(0), json!(1.5), json!(999_999), json!(true), json!([ids[0].0]), rect] {
        let e = s.execute(C, &json!({"id": bad})).unwrap_err().to_string();
        assert!(e.contains("id must name an image"), "{bad}: {e}");
        assert_eq!(s.doc().unwrap().history.undo.len(), undo, "{bad}: nothing changed");
        assert_eq!(*s.doc().unwrap().doc, *before, "{bad}: the selected image is left alone");
    }
    // Absent (or null): the selection.
    assert_eq!(s.execute(C, &json!({"id": null})).unwrap()["ids"], json!([ids[0].0]));
}

#[test]
fn a_mask_whose_image_has_transparency_of_its_own_is_refused() {
    let cases: [fn(&mut Document, &mut Node); 4] = [
        |_, art| art.opacity = 0.5,
        |_, art| art.blend = vectorcraft_color::BlendMode::Multiply,
        |d, art| {
            let inner = image_node(d, "mask", (7, 1), 0.0, 1.0);
            art.extras_mut().mask = Some(Box::new(OpacityMask::new(inner, true)));
        },
        |d, art| {
            // Even a disabled mask of the mask's own.
            let inner = image_node(d, "mask", (7, 1), 0.0, 1.0);
            let mut m = OpacityMask::new(inner, true);
            m.disabled = true;
            art.extras_mut().mask = Some(Box::new(m));
        },
    ];
    for (i, change) in cases.iter().enumerate() {
        let (mut s, ids, _) = masked_session(ALPHAS.to_vec(), 7, 1, false, 1);
        let id = ids[0];
        s.edit("t", |d, _| {
            let mut art = (*d.node(id).unwrap().extras().mask.as_ref().unwrap().art).clone();
            change(d, &mut art);
            d.node_mut(id).unwrap().extras_mut().mask.as_deref_mut().unwrap().art = Arc::new(art);
            Ok(())
        })
        .unwrap();
        select(&mut s, id);
        let before = s.doc().unwrap().doc.clone();
        let e = s.execute(C, &json!({})).unwrap_err().to_string();
        assert!(e.contains("the image's opacity mask has its own transparency: release it first"), "case {i}: {e}");
        assert_eq!(*s.doc().unwrap().doc, *before, "case {i}");
    }
}

#[test]
fn fully_opaque_own_alpha_under_a_mask_thresholds_the_mask() {
    let opaque = png(DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(7, 1, image::Rgba([1, 2, 3, 255]))));
    let (mut s, ids) = session_with(opaque, (7, 1), &[1.0]);
    let id = ids[0];
    s.edit("t", |d, _| {
        d.images.insert(
            "mask".into(),
            ImageBlob::new("image/png", png(DynamicImage::ImageLuma8(image::GrayImage::from_raw(7, 1, ALPHAS.to_vec()).unwrap()))),
        );
        let art = image_node(d, "mask", (7, 1), 0.0, 1.0);
        d.node_mut(id).unwrap().extras_mut().mask = Some(Box::new(OpacityMask::new(art, true)));
        Ok(())
    })
    .unwrap();
    select(&mut s, id);
    let r = s.execute(C, &json!({})).unwrap();
    assert_eq!((r["form"].clone(), counts(&r)), (json!("mask"), (5, 3, 4)));
    assert_eq!(key_of(&s, id), "src");
}
