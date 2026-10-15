//! Synthetic PDF resources only: two Form uses, each filling with a tiled RGB + SMask image.
use std::io::Cursor;

use serde_json::{Value, json};
use vectorcraft_doc::NodeKind;
use vectorcraft_geom::Affine;
use vectorcraft_testkit::pdf::{PdfPage, pdf_with};

use super::*;

fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
}

fn fixture(tiles: bool) -> Vec<u8> {
    let (w, h) = if tiles { (40, 60) } else { (20, 30) };
    let form = stream(
        &format!("/Type /XObject /Subtype /Form /BBox [0 0 {w} {h}] /Resources << /Pattern << /P 6 0 R >> >>"),
        &format!("/Pattern cs /P scn 0 0 {w} {h} re f"),
    );
    let pattern = stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 20 30] /XStep 20 /YStep 30 /Resources << /XObject << /I 7 0 R >> >>",
        "q 20 0 0 30 0 0 cm /I Do Q",
    );
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceRGB /SMask 8 0 R /Filter /ASCIIHexDecode",
        "ff002a123456abcdef000102>",
    );
    let mask =
        stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode", "0080ff01>");
    let page = PdfPage {
        resources: "/XObject << /F 5 0 R >>".into(),
        ..PdfPage::new(200.0, 150.0, "q 1 0 0 1 10 10 cm /F Do Q q 1 0 0 1 100 70 cm /F Do Q")
    };
    pdf_with(&[page], &[&form, &pattern, &image, &mask], None)
}

fn run(s: &mut Session, cmd: &str, p: Value) -> Value {
    s.execute(cmd, &p).unwrap_or_else(|e| panic!("{cmd}: {e}"))
}

fn open(tiles: bool) -> Session {
    let mut s = Session::new();
    run(
        &mut s,
        "document.open",
        json!({"name": "pattern.pdf", "dataBase64": vectorcraft_format::base64_encode(&fixture(tiles)), "editingData": false}),
    );
    s
}

fn listing(s: &mut Session) -> Value {
    run(s, "links.patternImages", json!({}))["images"].clone()
}

fn exported(s: &mut Session, id: &Value) -> Vec<u8> {
    let v = run(s, "links.exportPatternImage", json!({"id": id}));
    vectorcraft_format::base64_decode(v["dataBase64"].as_str().unwrap()).unwrap()
}

fn replacement() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(16, 16, |x, y| image::Rgba([17, 99, 231, if (x + y) % 2 == 0 { 255 } else { 0 }]));
    let mut bytes = vec![];
    img.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
    bytes
}

#[test]
fn pattern_images_preserve_native_rgb_and_soft_mask_samples() {
    for tiled in [false, true] {
        let mut s = open(tiled);
        assert_eq!(run(&mut s, "links.list", json!({}))["embedded"], 0);
        let rows = listing(&mut s);
        assert_eq!(rows.as_array().unwrap().len(), 1);
        let row = &rows[0];
        assert_eq!(row["useCount"], 2);
        assert_eq!(row["tileCount"], if tiled { 8 } else { 2 });
        assert_eq!(row["colorSpace"], "RGB");
        assert_eq!(row["hasAlpha"], true);
        assert_eq!(row["pixelWidth"], 2);
        assert_eq!(row["pixelHeight"], 2);
        let image = image::load_from_memory(&exported(&mut s, &row["id"])).unwrap().to_rgba8();
        assert_eq!(image.as_raw(), &[255, 0, 42, 0, 18, 52, 86, 128, 171, 205, 239, 255, 0, 1, 2, 1]);
        for p in row["placements"].as_array().unwrap() {
            let m: [f64; 6] = serde_json::from_value(p["transform"].clone()).unwrap();
            assert!((m[0].hypot(m[1]) - 20.0).abs() < 1e-6);
            assert!((m[2].hypot(m[3]) - 30.0).abs() < 1e-6);
            assert_eq!(p["artboards"], json!([1]));
        }
    }
}

#[test]
fn pattern_image_replacement_keeps_all_frames_and_round_trips() {
    let mut s = open(true);
    let before = listing(&mut s);
    let id = before[0]["id"].clone();
    let original = exported(&mut s, &id);
    let new = replacement();
    let layers = s.doc().unwrap().doc.layers.clone();
    run(&mut s, "links.replacePatternImage", json!({"id": id, "name": "dots.png", "dataBase64": vectorcraft_format::base64_encode(&new)}));
    let after = listing(&mut s);
    assert_eq!(after[0]["id"], id);
    assert_eq!(after[0]["placements"], before[0]["placements"]);
    assert_eq!(after[0]["pixelWidth"], 16);
    assert_eq!(exported(&mut s, &id), new);
    assert_eq!(s.doc().unwrap().doc.layers, layers, "no page geometry or paint changes");
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(listing(&mut s), before);
    assert_eq!(exported(&mut s, &id), original);
    run(&mut s, "edit.redo", json!({}));
    let bytes = vectorcraft_format::save_file(&s.doc().unwrap().doc);
    run(&mut s, "document.open", json!({"name": "roundtrip.vectorcraft", "dataBase64": vectorcraft_format::base64_encode(&bytes)}));
    assert_eq!(listing(&mut s), after);
    assert_eq!(exported(&mut s, &id), new);
    let pdf = vectorcraft_pdf::export(&s.doc().unwrap().doc, &Default::default()).unwrap();
    let d = vectorcraft_pdf::import(&pdf).unwrap();
    let expected = image::load_from_memory(&new).unwrap().to_rgba8();
    let mut images = 0;
    d.walk(|n| {
        if let NodeKind::Image(im) = &n.kind {
            let pixels = image::load_from_memory(&d.images[&im.key].bytes).unwrap().to_rgba8();
            assert_eq!(pixels, expected);
            images += 1;
        }
    });
    assert_eq!(images, 8, "all tiles contain replacement in the exported PDF");
}

#[test]
fn replacement_preserves_skew_mirror_and_nonuniform_pixel_scale() {
    let mut s = open(false);
    let row = listing(&mut s)[0].clone();
    let id = row["id"].as_u64().unwrap();
    let transform = Affine::new([-0.7, 0.3, 0.2, 1.8, 15.0, 19.0]);
    s.edit("fixture", |d, _| {
        for pat in d.patterns.iter_mut() {
            for art in &mut pat.art {
                fn change(n: &mut vectorcraft_doc::Node, id: u64, xf: Affine) {
                    if n.id.0 == id
                        && let NodeKind::Image(im) = &mut n.kind
                    {
                        im.xf = xf;
                    }
                    if let Some(ch) = n.children_mut() {
                        for c in ch {
                            change(std::sync::Arc::make_mut(c), id, xf);
                        }
                    }
                }
                change(std::sync::Arc::make_mut(art), id, transform);
            }
        }
        Ok(())
    })
    .unwrap();
    run(&mut s, "links.replacePatternImage", json!({"id": id, "dataBase64": vectorcraft_format::base64_encode(&replacement())}));
    for p in s.doc().unwrap().doc.patterns.iter() {
        for a in &p.art {
            a.walk(&mut |n| {
                if n.id.0 == id
                    && let NodeKind::Image(im) = &n.kind
                {
                    assert_eq!(im.xf * Affine::scale_non_uniform(im.width as f64, im.height as f64), transform * Affine::scale(2.0));
                }
            });
        }
    }
}

#[test]
fn bad_pattern_image_replacements_leave_document_unchanged() {
    let mut s = open(false);
    let row = listing(&mut s)[0].clone();
    let before = vectorcraft_format::save(&s.doc().unwrap().doc, false);
    for (id, data) in [(json!(999999), replacement()), (row["id"].clone(), b"not a png".to_vec()), (row["id"].clone(), replacement()[..40].to_vec())]
    {
        assert!(s.execute("links.replacePatternImage", &json!({"id": id, "dataBase64": vectorcraft_format::base64_encode(&data)})).is_err());
        assert_eq!(vectorcraft_format::save(&s.doc().unwrap().doc, false), before);
    }
    assert!(s.execute("links.exportPatternImage", &json!({"id": 999999})).is_err());
}

#[test]
fn shared_pixels_in_distinct_definitions_replace_together() {
    let mut s = open(false);
    s.edit("fixture", |d, _| {
        let mut second = d.patterns[0].clone();
        second.name = "Another tile".into();
        fn fresh(n: &mut vectorcraft_doc::Node, d: &mut vectorcraft_doc::Document) {
            n.id = d.alloc_id();
            if let Some(ch) = n.children_mut() {
                for c in ch {
                    fresh(std::sync::Arc::make_mut(c), d);
                }
            }
        }
        for a in &mut second.art {
            fresh(std::sync::Arc::make_mut(a), d);
        }
        d.patterns.push(second);
        Ok(())
    })
    .unwrap();
    let row = listing(&mut s)[0].clone();
    assert_eq!(row["ids"].as_array().unwrap().len(), 2);
    assert_eq!(row["patterns"].as_array().unwrap().len(), 2);
    let bytes = replacement();
    let changed = run(&mut s, "links.replacePatternImage", json!({"id": row["ids"][1], "dataBase64": vectorcraft_format::base64_encode(&bytes)}));
    assert_eq!(changed["updated"], 2);
    for id in row["ids"].as_array().unwrap() {
        assert_eq!(exported(&mut s, id), bytes);
    }
    assert_eq!(listing(&mut s)[0]["ids"], row["ids"]);
}
