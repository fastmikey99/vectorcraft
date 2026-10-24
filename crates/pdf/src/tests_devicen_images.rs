//! Images in DeviceN and Separation spaces with spot inks keep each spot ink as a channel printing
//! on its plate, beside their process inks, with their soft masks: on import (sharing the spot
//! swatches of paints in the same inks), and on export as DeviceN images with the same colorants.
//! Images in inks that are still read as RGB say why.

use std::sync::Arc;

use craft_pdf::syntax::Pdf;
use craft_pdf::syntax::object::{Array, Name, Object};
use vectorcraft_doc::{Document, ImageBlob, NodeKind};

use crate::*;

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

/// A 300 × 100 pt page painting a rectangle in the Separation space `/CS0` (object 6) at 100 %,
/// then the image XObject `image` (object 5) 50 pt wide, then `extra` objects (7…).
fn pdf(image: (&str, &[u8]), extra: &[Vec<u8>]) -> Vec<u8> {
    let content = "/CS0 cs 1 scn 0 80 300 20 re f\nq 50 0 0 50 10 10 cm /Im0 Do Q\n";
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Resources << /XObject << /Im0 5 0 R >> /ColorSpace << /CS0 6 0 R >> >> /Contents 4 0 R >>".to_vec(),
        stream("", content.as_bytes()),
        stream(&format!("/Type /XObject /Subtype /Image {}", image.0), image.1),
        b"[/Separation /Spot#20A /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>]".to_vec(),
    ];
    objs.extend_from_slice(extra);
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

/// The tint transform of `[/Cyan /Magenta /Yellow /Black /Spot#20A /Spot#20B]`: Spot A shows as
/// magenta, Spot B as yellow (object 7).
fn cmyk_ab_tint() -> Vec<u8> {
    stream(
        "/FunctionType 4 /Domain [0 1 0 1 0 1 0 1 0 1 0 1] /Range [0 1 0 1 0 1 0 1]",
        b"{ 5 index 5 index 3 index add 5 index 3 index add 5 index 10 4 roll pop pop pop pop pop pop }",
    )
}

/// A soft mask of three pixels: opaque, half, clear (object 8).
fn smask() -> Vec<u8> {
    stream("/Type /XObject /Subtype /Image /Width 3 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", &[255, 128, 0])
}

const CMYK_AB: &str = "[/DeviceN [/Cyan /Magenta /Yellow /Black /Spot#20A /Spot#20B] /DeviceCMYK 7 0 R]";

/// Three pixels: cyan; Spot A; Spot B at half with a quarter of magenta.
const SAMPLES: [u8; 18] = [255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 64, 0, 0, 0, 128];

fn image_nodes(d: &Document) -> Vec<&vectorcraft_doc::Node> {
    let mut out = vec![];
    for l in &d.layers {
        l.walk(&mut |n| {
            if matches!(n.kind, NodeKind::Image(_)) {
                out.push(n);
            }
        });
    }
    out
}

fn blob<'d>(d: &'d Document, n: &vectorcraft_doc::Node) -> &'d ImageBlob {
    let NodeKind::Image(im) = &n.kind else { panic!() };
    &d.images[&im.key]
}

fn spot_swatches(d: &Document) -> Vec<String> {
    d.swatches_iter().filter(|s| s.spot).map(|s| s.name.clone()).collect()
}

#[test]
fn a_devicen_image_keeps_its_spot_channels_and_mask_and_shares_the_paints_swatches() {
    let dict = format!("/Width 3 /Height 1 /BitsPerComponent 8 /ColorSpace {CMYK_AB} /SMask 8 0 R");
    let r = import_with_report(&pdf((&dict, &SAMPLES), &[cmyk_ab_tint(), smask()]), &ImportOptions::default()).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let d = &r.document;
    // The fill's Spot A and the image's Spot A are one swatch.
    let mut spots = spot_swatches(d);
    spots.sort();
    assert_eq!(spots, ["Spot A", "Spot B"]);
    let images = image_nodes(d);
    assert_eq!(images.len(), 1);
    let b = blob(d, images[0]);
    assert_eq!(b.cmyk().unwrap().data, [255, 0, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0]);
    let spots = b.spot_inks().unwrap().unwrap();
    assert_eq!(spots.names, ["Spot A", "Spot B"]);
    assert_eq!(spots.data, [0, 0, 255, 0, 0, 128]);
    // The soft mask is the image's opacity mask, as a CMYK image's is.
    let mask = images[0].extras().mask.as_ref().expect("the soft mask is kept");
    let NodeKind::Image(m) = &mask.art.kind else { panic!("mask art is an image") };
    let alpha = image::load_from_memory(&d.images[&m.key].bytes).unwrap().into_luma8().into_raw();
    assert_eq!(alpha, [255, 128, 0]);

    // Exported: a DeviceN image of the same colorants and samples, with its soft mask.
    let out = export(d, &PdfOptions::default()).unwrap();
    let file = Pdf::new(Arc::new(out.clone())).unwrap();
    let mut found = vec![];
    for o in file.objects() {
        let Object::Stream(s) = o else { continue };
        let dict = s.dict();
        if dict.get::<Name<'_>>(b"Subtype").is_none_or(|n| n.as_ref() != b"Image") {
            continue;
        }
        let Some(cs) = dict.get::<Array<'_>>(b"ColorSpace") else { continue };
        let mut it = cs.flex_iter();
        if it.next::<Name<'_>>().is_none_or(|n| n.as_ref() != b"DeviceN") {
            continue;
        }
        let names: Vec<String> = it.next::<Array<'_>>().unwrap().iter::<Name<'_>>().map(|n| n.as_str().to_string()).collect();
        let mask = dict.get::<craft_pdf::syntax::object::Stream<'_>>(b"SMask").map(|m| m.decoded().unwrap().to_vec());
        found.push((names, dict.get::<u32>(b"Width"), s.decoded().unwrap().to_vec(), mask));
    }
    assert_eq!(found.len(), 1, "one DeviceN image");
    let (names, width, data, mask) = &found[0];
    assert_eq!(names, &["Cyan", "Magenta", "Yellow", "Black", "Spot A", "Spot B"]);
    assert_eq!(*width, Some(3));
    assert_eq!(data.as_slice(), &SAMPLES);
    assert_eq!(mask.as_deref(), Some(&[255u8, 128, 0][..]));
    assert!(!out.windows(20).any(|w| w == b"VectorCraft DeviceN:"), "no stand-in is left");

    // And it opens again the same.
    let back = import_with_report(&out, &ImportOptions::default()).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let images = image_nodes(&back.document);
    let b = blob(&back.document, images[0]);
    assert_eq!(b.cmyk().unwrap().data, [255, 0, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0]);
    assert_eq!(b.spot_inks().unwrap().unwrap().data, [0, 0, 255, 0, 0, 128]);
    assert!(images[0].extras().mask.is_some());
    assert_eq!(spot_swatches(&back.document).len(), 2);
}

#[test]
fn separation_images_and_all_and_none_channels_print_on_their_plates() {
    // A Separation image in the fill's ink: its spot channel alone.
    let dict = "/Width 3 /Height 1 /BitsPerComponent 8 /ColorSpace 6 0 R";
    let r = import_with_report(&pdf((dict, &[0, 128, 255]), &[]), &ImportOptions::default()).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let b = blob(&r.document, image_nodes(&r.document)[0]);
    assert_eq!(b.cmyk().unwrap().data, [0; 12]);
    let s = b.spot_inks().unwrap().unwrap();
    assert_eq!((s.names.as_slice(), s.data.as_slice()), (&["Spot A".to_string()][..], &[0, 128, 255][..]));
    assert_eq!(spot_swatches(&r.document), ["Spot A"]);

    // /All must include document spots not named in the image. The current raster
    // model cannot retain that semantic, so production must receive a fatal diagnostic.
    for space in [
        "[/DeviceN [/Spot#20A /All /None] /DeviceCMYK 7 0 R]",
        "[/Separation /All /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 1 1 1] /N 1 >>]",
    ] {
        let tint = stream("/FunctionType 4 /Domain [0 1 0 1 0 1] /Range [0 1 0 1 0 1 0 1]", b"{ pop pop pop 0 0 0 0 }");
        let dict = format!("/Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace {space}");
        let r = import_with_report(&pdf((&dict, &[200, 0, 255, 0, 51, 255]), &[tint]), &ImportOptions::default()).unwrap();
        assert!(spot_swatches(&r.document).contains(&"Spot A".into()));
        let warning = r.warnings.iter().find(|w| w.contains("registration /All")).unwrap();
        assert_eq!(classify_diagnostic(warning), DiagnosticClass::FidelityLoss);
    }
}

#[test]
fn images_in_inks_read_as_rgb_say_why() {
    // A spot ink whose tint transform can't be read.
    let space = "[/DeviceN [/Cyan /Spot#20C] /DeviceCMYK 7 0 R]";
    let tint = b"<< /FunctionType 9 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] >>".to_vec();
    let dict = format!("/Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace {space}");
    let r = import_with_report(&pdf((&dict, &[0, 255]), &[tint]), &ImportOptions::default()).unwrap();
    assert!(r.warnings.iter().any(|w| w.contains("spot inks whose colour couldn't be read")), "{:?}", r.warnings);
    assert!(image_nodes(&r.document).iter().all(|n| blob(&r.document, n).spot_names().is_empty()));

    // An indexed image whose palette is in spot inks.
    let dict = "/Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace [/Indexed 6 0 R 1 <00FF>]";
    let r = import_with_report(&pdf((dict, &[0, 1]), &[]), &ImportOptions::default()).unwrap();
    assert!(r.warnings.iter().any(|w| w.contains("indexed images in CMYK or spot inks")), "{:?}", r.warnings);
}

#[test]
fn spot_decode_limit_stops_pdf_export_instead_of_falling_back_to_process() {
    let mut d = Document::new(100., 100.);
    let blob = vectorcraft_testkit::tiff::oversized_spot_image();
    assert_eq!(blob.spot_names().len(), 28);
    assert!(blob.spot_inks().unwrap_err().to_string().contains("decoded ink bytes"));
    d.images.insert("oversized".into(), blob);
    let mut node = vectorcraft_doc::Node::group(d.alloc_id(), vec![]);
    node.kind = NodeKind::Image(Box::new(vectorcraft_doc::ImageObject {
        key: "oversized".into(),
        width: 1,
        height: 1,
        xf: vectorcraft_geom::Affine::scale(10.),
        link: None,
        placement: Default::default(),
    }));
    d.insert(Some(d.layers[0].id), 0, node).unwrap();
    let err = export(&d, &PdfOptions::default()).unwrap_err();
    assert!(err.to_string().contains("decoded ink bytes"), "{err}");
}
