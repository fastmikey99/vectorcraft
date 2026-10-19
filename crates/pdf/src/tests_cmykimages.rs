//! CMYK images keep their CMYK samples from import to export (issue #320): a CMYK JPEG passes
//! through unchanged, other CMYK images keep their ink amounts, and only the Output settings
//! convert them.

use std::sync::Arc;

use craft_pdf::syntax::Pdf;
use craft_pdf::syntax::object::{Array, Name, Object, Stream};
use serde_json::json;
use vectorcraft_color::cms::{self, DEVICE_CMYK, GENERIC_CMYK, SRGB};
use vectorcraft_doc::cmyk::Inks;
use vectorcraft_doc::{ColorMode, Document, ImageBlob, ImageObject, Node, NodeKind};
use vectorcraft_geom::Affine;

use crate::*;

/// `inks` (four bytes a pixel) as a CMYK JPEG: Adobe's inverted samples, with its marker.
fn cmyk_jpeg(inks: &[u8], w: u16, h: u16) -> Vec<u8> {
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 100).encode(inks, w, h, jpeg_encoder::ColorType::Cmyk).unwrap();
    out
}

fn flat(inks: [u8; 4], n: usize) -> Vec<u8> {
    inks.repeat(n)
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

/// A 300 × 100 pt page with a CMYK rectangle and image XObjects `images` (dictionary entries
/// after the type, stream data), 50 pt wide each: objects 5…, then the `extra` objects.
fn pdf_with_images(images: &[(String, Vec<u8>)], extra: &[Vec<u8>]) -> Vec<u8> {
    pdf_with_image_resources(images, extra, "")
}

fn pdf_with_image_resources(images: &[(String, Vec<u8>)], extra: &[Vec<u8>], resources: &str) -> Vec<u8> {
    pdf_with_image_scope(images, extra, resources, None)
}

fn pdf_with_image_scope(images: &[(String, Vec<u8>)], extra: &[Vec<u8>], resources: &str, form_resources: Option<&str>) -> Vec<u8> {
    let xobjects: String = (0..images.len()).map(|i| format!("/Im{i} {} 0 R ", 5 + i)).collect();
    let mut content = String::from("0 1 0.6 0.2 k 0 80 300 20 re f\n");
    for i in 0..images.len() {
        content.push_str(&format!("q 50 0 0 50 {} 10 cm /Im{i} Do Q\n", 10 + 60 * i));
    }
    let form_id = 5 + images.len() + extra.len();
    let page_xobjects = if form_resources.is_some() { format!("/F {form_id} 0 R") } else { xobjects.clone() };
    let page_content = if form_resources.is_some() { "/F Do" } else { &content };
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Resources << {resources} /XObject << {page_xobjects}>> >> /Contents 4 0 R >>")
            .into_bytes(),
        stream("", page_content.as_bytes()),
    ];
    for (dict, data) in images {
        objs.push(stream(&format!("/Type /XObject /Subtype /Image {dict}"), data));
    }
    objs.extend_from_slice(extra);
    if let Some(form_resources) = form_resources {
        objs.push(stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 300 100] /Resources << {form_resources} /XObject << {xobjects} >> >>"),
            content.as_bytes(),
        ));
    }
    pdf_objects(&objs)
}

fn pdf_objects(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut at = vec![];
    for (i, o) in objs.iter().enumerate() {
        at.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for a in at {
        out.extend_from_slice(format!("{a:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// An image dictionary's entries: `w` × `h` CMYK pixels, with `more`.
fn cmyk_dict(w: u32, h: u32, more: &str) -> String {
    format!("/Width {w} /Height {h} /ColorSpace /DeviceCMYK /BitsPerComponent 8 {more}")
}

const INVERTED: &str = "/Decode [1 0 1 0 1 0 1 0]";

#[test]
fn hard_edge_export_disables_interpolation_on_images_and_their_masks() {
    let mask = stream("/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0, 255]);
    let input = pdf_with_images(&[(cmyk_dict(2, 1, "/SMask 6 0 R"), vec![255, 0, 0, 0, 0, 255, 0, 0])], &[mask]);
    let doc = import(&input).unwrap();
    for hard in [true, false] {
        let opts = PdfOptions { settings: PdfSettings { disable_image_interpolation: hard, ..Default::default() }, ..Default::default() };
        let out = export(&doc, &opts).unwrap();
        let file = Pdf::new(Arc::new(out)).unwrap();
        let mut count = 0;
        for object in file.objects() {
            if let Object::Stream(s) = object
                && s.dict().get::<Name<'_>>(b"Subtype").is_some_and(|n| n.as_str() == "Image")
            {
                assert_eq!(s.dict().get::<bool>(b"Interpolate").unwrap_or(false), !hard);
                count += 1;
            }
        }
        assert_eq!(count, 2, "the colour image and opacity-mask image both survive");
    }
}

/// An image XObject of a written PDF: its colour space and the components of its ICC profile,
/// its Decode array, its filter and its stream data.
#[derive(Debug)]
struct Written {
    space: String,
    icc_n: Option<u8>,
    decode: Vec<f32>,
    filter: Option<String>,
    data: Vec<u8>,
    width: u32,
}

fn written(pdf: &[u8]) -> Vec<Written> {
    let file = Pdf::new(Arc::new(pdf.to_vec())).unwrap();
    let mut out = vec![];
    for o in file.objects() {
        let Object::Stream(s) = o else { continue };
        let d = s.dict();
        if d.get::<Name<'_>>(b"Subtype").is_none_or(|n| n.as_ref() != b"Image") {
            continue;
        }
        let (space, icc_n) = match (d.get::<Name<'_>>(b"ColorSpace"), d.get::<Array<'_>>(b"ColorSpace")) {
            (Some(n), _) => (n.as_str().to_string(), None),
            (_, Some(a)) => {
                let mut it = a.flex_iter();
                let name = it.next::<Name<'_>>().unwrap().as_str().to_string();
                (name, it.next::<Stream<'_>>().and_then(|s| s.dict().get::<u8>(b"N")))
            }
            _ => (String::new(), None),
        };
        out.push(Written {
            space,
            icc_n,
            decode: d.get::<Array<'_>>(b"Decode").map(|a| a.iter::<f32>().collect()).unwrap_or_default(),
            filter: d.get::<Name<'_>>(b"Filter").map(|n| n.as_str().to_string()),
            data: s.raw_data().to_vec(),
            width: d.get::<u32>(b"Width").unwrap_or(0),
        });
    }
    out
}

/// The blob each image of `d` shows, in paint order.
fn blobs(d: &Document) -> Vec<&ImageBlob> {
    let NodeKind::Layer { children, .. } = &d.layers[0].kind else { panic!() };
    children
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Image(im) => d.images.get(&im.key),
            _ => None,
        })
        .collect()
}

/// Options with settings `s` (at their standard's version).
fn opts(s: serde_json::Value) -> PdfOptions {
    let settings: PdfSettings = serde_json::from_value(s).unwrap();
    let compatibility = if settings.standard == Standard::None { settings.compatibility } else { settings.standard.version() };
    PdfOptions { settings: PdfSettings { compatibility, ..settings }, created: Some(1_700_000_000), ..Default::default() }
}

fn close(a: &[u8], b: &[u8], tolerance: u8) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= tolerance)
}

#[test]
fn indexed_cmyk_keeps_palette_inks_decode_row_padding_and_alpha() {
    let palette = [0, 0, 0, 0, 10, 20, 30, 40, 255, 0, 99, 0, 0, 255, 0, 17];
    // Three pixels per row: two-bit samples have two padding bits on each row.
    let image = "/Width 3 /Height 2 /ColorSpace [/Indexed /DeviceCMYK 3 6 0 R] /BitsPerComponent 2 /Decode [3 0] /SMask 7 0 R";
    let mask = stream("/Type /XObject /Subtype /Image /Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0, 51, 102, 153, 204, 255]);
    let input = pdf_with_images(&[(image.into(), vec![0b00011011, 0b11100111])], &[stream("", &palette), mask]);
    let imported = import_with_report(&input, &ImportOptions::default()).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let expected: Vec<u8> = [3, 2, 1, 0, 1, 2].into_iter().flat_map(|i| palette[i * 4..i * 4 + 4].iter().copied()).collect();
    for doc in [&imported.document, &import(&export(&imported.document, &PdfOptions::default()).unwrap()).unwrap()] {
        let images = masked(doc);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].0.cmyk().unwrap().data, expected);
        assert!(images[0].1.is_some(), "opacity mask retained");
    }
}

#[test]
fn indexed_cmyk_string_palette_clamps_indices_and_reports_short_tables() {
    let dict = "/Width 3 /Height 1 /ColorSpace [/Indexed /DeviceCMYK 1 <000000000a141e28>] /BitsPerComponent 8";
    let input = pdf_with_images(&[(dict.into(), vec![0, 1, 255])], &[]);
    let doc = import(&input).unwrap();
    assert_eq!(blobs(&doc)[0].cmyk().unwrap().data, [0, 0, 0, 0, 10, 20, 30, 40, 10, 20, 30, 40]);
    let input = pdf_with_images(&[(dict.replace("000000000a141e28", "00000000"), vec![0, 1, 255])], &[]);
    let result = import_with_report(&input, &ImportOptions::default()).unwrap();
    assert!(result.warnings.iter().any(|w| w.contains(crate::import_image::UNREAD)));
}

#[test]
fn device_gray_images_keep_process_samples_and_alpha_through_export_and_reopen() {
    let mask = stream("/Type /XObject /Subtype /Image /Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0, 51, 102, 153, 204, 255]);
    for (space, bpc, decode, data) in [
        ("/DeviceGray", 8, "", vec![0, 64, 255, 128, 32, 192]),
        ("/DeviceGray", 8, "/Decode [1 0]", vec![0, 64, 255, 128, 32, 192]),
        ("[/Indexed /DeviceGray 3 <004080ff>]", 2, "/Decode [3 0]", vec![0b00011011, 0b11100111]),
    ] {
        for masked in [false, true] {
            let dict =
                format!("/Width 3 /Height 2 /ColorSpace {space} /BitsPerComponent {bpc} {decode} {}", if masked { "/SMask 6 0 R" } else { "" });
            let input = pdf_with_images(&[(dict, data.clone())], if masked { std::slice::from_ref(&mask) } else { &[] });
            let source = print_order(&input).unwrap();
            let expected = source.images[0][0].samples.as_ref().unwrap();
            let mut doc = import(&input).unwrap();
            for _ in 0..2 {
                let bytes = export(&doc, &opts(json!({"disableImageInterpolation": true}))).unwrap();
                let output = print_order(&bytes).unwrap();
                assert_eq!(output.images[0].len(), 1);
                let image = &output.images[0][0];
                assert_eq!(image.description.colour_space, "DeviceGray", "{space}, masked={masked}");
                let actual = image.samples.as_ref().unwrap();
                assert_eq!(actual.process_base(), Some("DeviceGray"));
                assert_eq!((actual.width, actual.height), (3, 2));
                for pixel in 0..6 {
                    assert_eq!(actual.tone(pixel, 0).unwrap(), expected.tone(pixel, 0).unwrap());
                    assert_eq!(actual.alpha(pixel).unwrap(), expected.alpha(pixel).unwrap());
                }
                doc = import(&bytes).unwrap();
            }
        }
    }
}

#[test]
fn neutral_rgb_named_black_and_gray_substitutions_are_not_relabelled_device_gray() {
    let function = b"<< /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [0 0 0] /N 1 >>".to_vec();
    let gray_profile = stream("/N 1", &cms::icc_bytes(cms::GRAY).unwrap());
    for (space, data, extra, resources) in [
        ("/DeviceRGB", vec![0, 0, 0, 64, 64, 64, 255, 255, 255], vec![], ""),
        ("[/Separation /Black /DeviceRGB 6 0 R]", vec![0, 64, 255], vec![function], ""),
        ("[/CalGray << /WhitePoint [1 1 1] >>]", vec![0, 64, 255], vec![], ""),
        ("[/ICCBased 6 0 R]", vec![0, 64, 255], vec![gray_profile], ""),
        ("/DeviceGray", vec![0, 64, 255], vec![], "/ColorSpace << /DefaultGray [/CalGray << /WhitePoint [1 1 1] >>] >>"),
        ("[/Indexed /DeviceGray 2 <0040ff>]", vec![0, 1, 2], vec![], "/ColorSpace << /DefaultGray null >>"),
    ] {
        let dict = format!("/Width 3 /Height 1 /ColorSpace {space} /BitsPerComponent 8");
        let input = pdf_with_image_resources(&[(dict, data)], &extra, resources);
        let bytes = export(&import(&input).unwrap(), &PdfOptions::default()).unwrap();
        let output = print_order(&bytes).unwrap();
        let expected = if space.starts_with("[/Separation") { "DeviceCMYK" } else { "DeviceRGB" };
        assert_eq!(output.images[0][0].description.colour_space, expected, "{space} {resources}");
    }
}

#[test]
fn gray_preservation_respects_form_and_inherited_resource_substitutions() {
    let image = ("/Width 3 /Height 1 /ColorSpace [/Indexed /DeviceGray 2 <0040ff>] /BitsPerComponent 8".into(), vec![0, 1, 2]);
    let substitution = "/ColorSpace << /DefaultGray [/CalGray << /WhitePoint [1 1 1] >>] >>";
    for (page, form, expected) in [("", "", "DeviceGray"), (substitution, "", "DeviceRGB"), ("", substitution, "DeviceRGB")] {
        let input = pdf_with_image_scope(std::slice::from_ref(&image), &[], page, Some(form));
        let bytes = export(&import(&input).unwrap(), &PdfOptions::default()).unwrap();
        let output = print_order(&bytes).unwrap();
        assert_eq!(output.images[0][0].description.colour_space, expected, "page={page}, form={form}");
    }
}

#[test]
fn gray_preservation_cache_isolated_for_shared_image_across_pages_in_both_orders() {
    let substitution = "/ColorSpace << /DefaultGray [/CalGray << /WhitePoint [1 1 1] >>] >>";
    for overrides in [["", substitution], [substitution, ""]] {
        let page = |resources: &str| {
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Contents 5 0 R /Resources << {resources} /XObject << /Im 6 0 R >> >> >>")
                .into_bytes()
        };
        let input = pdf_objects(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
            page(overrides[0]),
            page(overrides[1]),
            stream("", b"q 36 0 0 36 18 18 cm /Im Do Q"),
            stream("/Type /XObject /Subtype /Image /Width 3 /Height 1 /ColorSpace [/Indexed /DeviceGray 2 <0040ff>] /BitsPerComponent 8", &[0, 1, 2]),
        ]);
        let output = export(&import(&input).unwrap(), &PdfOptions::default()).unwrap();
        let inventory = print_order(&output).unwrap();
        assert_eq!(inventory.images.len(), 2);
        for (images, declaration) in inventory.images.iter().zip(overrides) {
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].description.colour_space, if declaration.is_empty() { "DeviceGray" } else { "DeviceRGB" });
        }
    }
}

#[test]
fn a_cmyk_jpeg_passes_through_open_and_every_pdf_export() {
    // The issue's file: a DeviceCMYK JPEG (Adobe's inverted samples, Decode [1 0 …]) beside a
    // DeviceCMYK fill.
    let jpg = cmyk_jpeg(&flat([0, 102, 255, 0], 32 * 16), 32, 16);
    let source = pdf_with_images(&[(cmyk_dict(32, 16, &format!("/Filter /DCTDecode {INVERTED}")), jpg.clone())], &[]);
    let r = import_with_report(&source, &ImportOptions::default()).unwrap();
    assert_eq!(r.document.color_mode, ColorMode::Cmyk);
    let b = blobs(&r.document);
    assert_eq!(b.len(), 1);
    assert_eq!((b[0].mime.as_str(), b[0].bytes.as_slice()), ("image/jpeg", jpg.as_slice()), "the JPEG as it is");
    assert!(b[0].cmyk().unwrap().data.as_chunks::<4>().0.iter().all(|p| close(p, &[0, 102, 255, 0], 2)));
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);

    for (settings, space, icc_n) in [
        (json!({}), "DeviceCMYK", None),
        (json!({"output": {"conversion": "preserveNumbers"}}), "DeviceCMYK", None),
        (json!({"standard": "pdfX1a"}), "DeviceCMYK", None),
        (json!({"standard": "pdfX3"}), "ICCBased", Some(4)),
        (json!({"standard": "pdfX4"}), "ICCBased", Some(4)),
        (json!({"output": {"profiles": "all"}}), "ICCBased", Some(4)),
    ] {
        let out = export_with_report(&r.document, &opts(settings.clone())).unwrap();
        let w = written(&out.bytes);
        assert_eq!(w.len(), 1, "{settings}: {w:?}");
        assert_eq!((w[0].space.as_str(), w[0].icc_n), (space, icc_n), "{settings}");
        assert_eq!(w[0].filter.as_deref(), Some("DCTDecode"), "{settings}");
        assert_eq!(w[0].decode, [1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0], "{settings}");
        assert!(w[0].data == jpg, "{settings}: the JPEG data unchanged");
        assert!(out.warnings.iter().all(|w| !w.contains("image")), "{settings}: {:?}", out.warnings);
        // And open again: the same JPEG.
        let back = import(&out.bytes).unwrap();
        assert_eq!(blobs(&back)[0].bytes.as_slice(), jpg.as_slice(), "{settings}");
    }
}

#[test]
fn other_cmyk_images_keep_their_ink_amounts() {
    let inks: Vec<u8> = [[0, 0, 0, 0], [255, 0, 0, 0], [10, 20, 30, 40], [0, 102, 255, 0]].concat();
    let z = |data: &[u8]| crate::output::deflate(data).unwrap();
    let inverted: Vec<u8> = inks.iter().map(|v| 255 - v).collect();
    let wide: Vec<u8> = inks.iter().flat_map(|v| (*v as u16 * 257).to_be_bytes()).collect();
    let images = [
        (cmyk_dict(2, 2, "/Filter /FlateDecode"), z(&inks)),
        (cmyk_dict(2, 2, &format!("/Filter /FlateDecode {INVERTED}")), z(&inverted)),
        (cmyk_dict(2, 2, "/Filter /FlateDecode").replace("/BitsPerComponent 8", "/BitsPerComponent 16"), z(&wide)),
        // A CMYK JPEG holding the ink amounts themselves (no Decode array): not Adobe's convention.
        (cmyk_dict(2, 2, "/Filter /DCTDecode"), cmyk_jpeg(&inverted, 2, 2)),
        // ICC-based with four components (object 10: after the page's 4 objects and 5 images).
        (cmyk_dict(2, 2, "/Filter /FlateDecode").replace("/DeviceCMYK", "[/ICCBased 10 0 R]"), z(&inks)),
    ];
    let source = pdf_with_images(&images, &[stream("/N 4", b"")]);
    let r = import_with_report(&source, &ImportOptions::default()).unwrap();
    let b = blobs(&r.document);
    assert_eq!(b.len(), 5, "{:?}", r.warnings);
    for (i, blob) in b.iter().enumerate() {
        assert_eq!(blob.mime, "image/tiff", "image {i}");
        let got = blob.cmyk().unwrap();
        assert_eq!((got.width, got.height), (2, 2));
        let tolerance = if i == 3 { 3 } else { 0 };
        assert!(close(&got.data, &inks, tolerance), "image {i}: {:?}", got.data);
    }
    // Written as CMYK, losslessly: they open with the same ink amounts.
    let out = export_with_report(&r.document, &opts(json!({}))).unwrap();
    let w = written(&out.bytes);
    assert!(!w.is_empty() && w.iter().all(|w| w.space == "DeviceCMYK" && w.decode.is_empty()), "{w:?}");
    let back = import(&out.bytes).unwrap();
    for blob in blobs(&back) {
        assert!(close(&blob.cmyk().unwrap().data, &inks, 3));
    }
}

#[test]
fn cmyk_images_with_odd_samples_open_in_rgb_with_a_warning() {
    let four_bits = cmyk_dict(2, 2, "").replace("/BitsPerComponent 8", "/BitsPerComponent 4");
    let r = import_with_report(&pdf_with_images(&[(four_bits, vec![0x0F; 8])], &[]), &ImportOptions::default()).unwrap();
    assert_eq!(blobs(&r.document)[0].mime, "image/png");
    assert!(r.warnings.iter().any(|w| w == crate::import_image::UNREAD), "{:?}", r.warnings);
}

/// An opacity mask's image: its blob and pixel size.
type MaskImage<'d> = (&'d ImageBlob, u32, u32);

/// The image nodes on the first layer: each one's blob and its opacity mask's image, if it has one.
fn masked(d: &Document) -> Vec<(&ImageBlob, Option<MaskImage<'_>>)> {
    let NodeKind::Layer { children, .. } = &d.layers[0].kind else { panic!() };
    children
        .iter()
        .filter_map(|n| {
            let NodeKind::Image(im) = &n.kind else { return None };
            let mask = n.extras().mask.as_ref().map(|m| match &m.art.kind {
                NodeKind::Image(a) => (&d.images[&a.key], a.width, a.height),
                other => panic!("mask art is an image: {other:?}"),
            });
            Some((&d.images[&im.key], mask))
        })
        .collect()
}

#[test]
fn masked_cmyk_images_keep_their_inks_and_carry_the_mask_as_an_opacity_mask() {
    let inks = flat([0, 102, 255, 0], 4);
    let z = |data: &[u8]| crate::output::deflate(data).unwrap();
    // Object 6 (the first after the two images): a soft mask; image 2 a colour key masking its
    // first two pixels.
    let smask = stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[255, 128, 255, 128]);
    let keyed: Vec<u8> = [[10, 20, 30, 40], [10, 20, 30, 40], [0, 102, 255, 0], [0, 102, 255, 0]].concat();
    let images = [
        (cmyk_dict(2, 2, "/SMask 7 0 R /Filter /FlateDecode"), z(&inks)),
        (cmyk_dict(2, 2, "/Mask [10 10 20 20 30 30 40 40] /Filter /FlateDecode"), z(&keyed)),
    ];
    let source = pdf_with_images(&images, &[smask]);
    let r = import_with_report(&source, &ImportOptions::default()).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let m = masked(&r.document);
    assert_eq!(m.len(), 2);
    for ((blob, mask), (want, alpha)) in m.iter().zip([(&inks, [255, 128, 255, 128]), (&keyed, [0, 0, 255, 255])]) {
        assert_eq!(blob.mime, "image/tiff");
        assert!(close(&blob.cmyk().unwrap().data, want, 0), "the inks as they were");
        let (mblob, mw, mh) = mask.expect("an opacity mask");
        assert_eq!((mw, mh), (2, 2));
        let grey = image::load_from_memory(&mblob.bytes).unwrap().to_luma8();
        assert_eq!(grey.into_raw(), alpha, "the mask's values");
    }
    // The mask art covers the image exactly.
    let NodeKind::Layer { children, .. } = &r.document.layers[0].kind else { panic!() };
    let img = children.iter().find(|n| matches!(n.kind, NodeKind::Image(_))).unwrap();
    assert_eq!(img.geometric_bounds(), img.extras().mask.as_ref().unwrap().art.geometric_bounds());
    // Written as CMYK with the mask in grey; it opens with the same inks.
    let out = export_with_report(&r.document, &opts(json!({}))).unwrap();
    let w = written(&out.bytes);
    assert!(w.iter().filter(|w| w.space == "DeviceCMYK").count() == 2 && w.iter().any(|w| w.space == "DeviceGray"), "{w:?}");
    let back = import_with_report(&out.bytes, &ImportOptions::default()).unwrap();
    let inked: Vec<Vec<u8>> = all_images(&back.document).iter().filter_map(|b| b.cmyk()).map(|i| i.data).collect();
    assert!(inked.iter().any(|d| close(d, &inks, 3)) && inked.iter().any(|d| close(d, &keyed, 3)), "{:?}", back.warnings);
    // A mask that hides nothing adds none.
    let opaque = stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[255; 4]);
    let r = import_with_report(
        &pdf_with_images(&[(cmyk_dict(2, 2, "/SMask 6 0 R /Filter /FlateDecode"), z(&inks))], &[opaque]),
        &ImportOptions::default(),
    )
    .unwrap();
    assert!(masked(&r.document)[0].1.is_none());
}

/// Every image blob a document's art uses (mask art included).
fn all_images(d: &Document) -> Vec<&ImageBlob> {
    let mut keys = vec![];
    d.walk(|n| {
        if let NodeKind::Image(im) = &n.kind {
            keys.push(im.key.clone());
        }
        if let Some(m) = &n.extras().mask {
            m.art.walk(&mut |a| {
                if let NodeKind::Image(im) = &a.kind {
                    keys.push(im.key.clone());
                }
            });
        }
    });
    keys.iter().filter_map(|k| d.images.get(k)).collect()
}

/// A document with CMYK image `blob` of `w` × `h` pixels placed 40 pt wide.
fn image_doc(blob: ImageBlob, w: u32, h: u32) -> Document {
    let mut d = Document::new_with_mode(100.0, 100.0, ColorMode::Cmyk);
    d.images.insert("img".into(), blob);
    let image = ImageObject { key: "img".into(), width: w, height: h, xf: Affine::scale(40.0 / w as f64), link: None, placement: Default::default() };
    let n = Node::new(d.alloc_id(), NodeKind::Image(Box::new(image)));
    let l = d.layers[0].id;
    d.insert(Some(l), 0, n).unwrap();
    d
}

#[test]
fn the_output_settings_convert_cmyk_images_and_compression_recompresses_them_in_cmyk() {
    let ink = [0u8, 102, 255, 0];
    let tiff = ImageBlob::cmyk_tiff(&Inks::new(4, 4, flat(ink, 16)).unwrap()).unwrap();
    let d = image_doc(tiff, 4, 4);
    let first = |s: serde_json::Value| {
        let out = export_with_report(&d, &opts(s.clone())).unwrap();
        let w = written(&out.bytes);
        assert_eq!(w.len(), 1, "{s}");
        let back = import(&out.bytes).unwrap();
        let blob = blobs(&back)[0].clone();
        (w.into_iter().next().unwrap(), blob, out.warnings)
    };
    // Kept: its numbers.
    for s in [json!({}), json!({"output": {"conversion": "preserveNumbers", "destination": DEVICE_CMYK}})] {
        let (w, blob, warnings) = first(s.clone());
        assert_eq!(w.space, "DeviceCMYK", "{s}");
        assert_eq!(blob.cmyk().unwrap().data, flat(ink, 16), "{s}");
        assert!(warnings.is_empty(), "{s}: {warnings:?}");
    }
    // To another CMYK profile: other numbers, still CMYK.
    let (w, blob, _) = first(json!({"output": {"conversion": "destination", "destination": DEVICE_CMYK}}));
    assert_eq!(w.space, "DeviceCMYK");
    let converted = blob.cmyk().unwrap().data;
    assert_ne!(converted, flat(ink, 16));
    let source = cms::active();
    let dest = cms::Cms::new(&cms::ColorSettings { cmyk: DEVICE_CMYK.into(), ..source.settings().clone() }).unwrap();
    let want = dest.lab_to_cmyk(source.cmyk_to_lab(ink.map(|v| v as f32 / 255.0)), source.settings().intent).map(|v| (v * 255.0).round() as u8);
    assert!(close(&converted[..4], &want, 3), "{converted:?} vs {want:?}");
    // To RGB: the colour shown on screen.
    let (w, blob, _) = first(json!({"output": {"conversion": "destination", "destination": SRGB}}));
    assert_eq!(w.space, "DeviceRGB");
    let shown = source.cmyk_to_srgb(ink.map(|v| v as f32 / 255.0), false).map(|v| (v * 255.0).round() as u8);
    let px = image::load_from_memory(&blob.bytes).unwrap().to_rgb8().get_pixel(0, 0).0;
    assert!(close(&px, &shown, 3), "{px:?} vs {shown:?}");
    // JPEG compression: a CMYK JPEG (Adobe's inverted samples, undone by the Decode array).
    let (w, blob, warnings) = first(json!({"compression": {"color": {"compression": "jpeg", "quality": "maximum"}}}));
    assert_eq!((w.space.as_str(), w.filter.as_deref()), ("DeviceCMYK", Some("DCTDecode")));
    assert_eq!(w.decode, [1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(blob.cmyk().unwrap().data.as_chunks::<4>().0.iter().all(|p| close(p, &ink, 3)));
    // Resampled: fewer pixels, the same inks.
    let jpg = ImageBlob::new("image/jpeg", cmyk_jpeg(&flat(ink, 64 * 64), 64, 64));
    let d = image_doc(jpg, 64, 64);
    let s = json!({"compression": {"color": {"downsample": "bicubic", "ppi": 72.0, "abovePpi": 100.0}}});
    let out = export_with_report(&d, &opts(s)).unwrap();
    let w = written(&out.bytes);
    assert_eq!((w[0].space.as_str(), w[0].width), ("DeviceCMYK", 40), "{w:?}");
    assert!(blobs(&import(&out.bytes).unwrap())[0].cmyk().unwrap().data.as_chunks::<4>().0.iter().all(|p| close(p, &ink, 3)));
    // And PDF/X-1a keeps the numbers of a CMYK image (no conversion through RGB).
    let (w, blob, _) = first(json!({"standard": "pdfX1a", "output": {"destination": GENERIC_CMYK}}));
    assert_eq!(w.space, "DeviceCMYK");
    assert_eq!(blob.cmyk().unwrap().data, flat(ink, 16));
}

#[test]
fn named_process_image_channels_survive_import_export_and_reopen() {
    // ISO 32000-1, Separation/DeviceN process colorant names. Deliberately reorder
    // the samples and leave channels out; the preview tint function is not ink data.
    for (space, samples, decode, expected) in [
        ("[/DeviceN [/Yellow /Cyan /Magenta] /DeviceCMYK 6 0 R]", vec![30, 10, 20, 90, 70, 80], "", vec![10, 20, 30, 0, 70, 80, 90, 0]),
        ("[/DeviceN [/Magenta /Yellow /Black] /DeviceCMYK 6 0 R]", vec![20, 30, 40, 80, 90, 100], "", vec![0, 20, 30, 40, 0, 80, 90, 100]),
        (
            "[/DeviceN [/Magenta /Yellow /Black] /DeviceCMYK 6 0 R]",
            vec![235, 225, 215, 175, 165, 155],
            "/Decode [1 0 1 0 1 0]",
            vec![0, 20, 30, 40, 0, 80, 90, 100],
        ),
    ] {
        let dict = format!("/Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace {space} {decode}");
        let tint = stream("/FunctionType 4 /Domain [0 1 0 1 0 1] /Range [0 1 0 1 0 1 0 1]", b"{ pop pop pop 0 0 0 0 }");
        let source = pdf_with_images(&[(dict, samples)], &[tint]);
        let imported = import_with_report(&source, &ImportOptions::default()).unwrap();
        assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
        assert_eq!(blobs(&imported.document)[0].cmyk().unwrap().data, expected);
        let bytes = export(&imported.document, &PdfOptions::default()).unwrap();
        let reopened = import(&bytes).unwrap();
        assert_eq!(blobs(&reopened)[0].cmyk().unwrap().data, expected);
    }
}

#[test]
fn sixteen_bit_named_process_samples_keep_their_ink_values() {
    let dict = "/Width 2 /Height 1 /BitsPerComponent 16 /ColorSpace [/Separation /Black /DeviceCMYK 6 0 R]";
    let tint = b"<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 1] /N 1 >>".to_vec();
    let source = pdf_with_images(&[(dict.into(), vec![0x40, 0x40, 0xC0, 0xC0])], &[tint]);
    let imported = import(&source).unwrap();
    assert_eq!(blobs(&imported)[0].cmyk().unwrap().data, [0, 0, 0, 64, 0, 0, 0, 192]);
}

#[test]
fn pdf_jpeg_decode_polarity_and_original_encoding_survive_export() {
    let jpg = cmyk_jpeg(&flat([30, 70, 120, 10], 16 * 16), 16, 16);
    let source = pdf_with_images(&[(cmyk_dict(16, 16, "/Filter /DCTDecode"), jpg.clone())], &[]);
    let mut doc = import(&source).unwrap();
    let blob = blobs(&doc)[0];
    assert_eq!(blob.source_jpeg.as_ref().unwrap().bytes.as_slice(), jpg);
    let working = blob.cmyk().unwrap();
    let out = export(&doc, &opts(json!({"compression":{"color":{"compression":"auto"}}}))).unwrap();
    let images = written(&out);
    assert_eq!(images[0].data, jpg, "compressed source bytes unchanged");
    assert!(images[0].decode.is_empty(), "PDF samples must not be inverted");
    assert_eq!(blobs(&import(&out).unwrap())[0].cmyk().unwrap(), working);
    // An edited working bitmap invalidates the preserved encoding, even if metadata was cloned.
    let key = doc.images.keys().next().unwrap().clone();
    let old = doc.images.get_mut(&key).unwrap();
    old.bytes = ImageBlob::cmyk_tiff(&Inks::new(16, 16, flat([0, 0, 0, 255], 256)).unwrap()).unwrap().bytes;
    let changed = export(&doc, &opts(json!({}))).unwrap();
    assert_ne!(written(&changed)[0].data, jpg);
    assert!(blobs(&import(&changed).unwrap())[0].cmyk().unwrap().data.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
}

#[test]
fn vector_cmyk_keeps_fractional_press_values() {
    let source = pdf_with_images(&[], &[]);
    let doc = import(&source).unwrap();
    let out = export(&doc, &PdfOptions::default()).unwrap();
    let back = import(&out).unwrap();
    fn colours(doc: &Document) -> Vec<vectorcraft_color::Color> {
        let mut result = vec![];
        doc.walk(|n| {
            if matches!(n.kind, NodeKind::Path { .. })
                && let Some(c) = n.appearance.fill_paint().color()
            {
                result.push(c);
            }
        });
        result
    }
    assert_eq!(colours(&back), colours(&doc));
}
