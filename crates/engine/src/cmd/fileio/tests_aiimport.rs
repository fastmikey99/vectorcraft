//! Illustrator EPS, `.ai` and PDF files open from the editing data they carry: layers, groups,
//! names, hidden objects, artboards, colours and images as they were; what the reader doesn't read
//! yet opens as before, with a warning saying why.

use std::io::Write as _;
use std::sync::Arc;

use serde_json::{Value, json};
use vectorcraft_doc::{Node, NodeKind};
use vectorcraft_testkit::ai;
use vectorcraft_testkit::pdf::{PdfPage, first_extra, pdf_with};

use super::*;

fn open(s: &mut Session, name: &str, bytes: &[u8], params: Value) -> Result<Value> {
    let mut p = json!({"name": name, "dataBase64": vectorcraft_format::base64_encode(bytes)});
    if let (Some(p), Some(extra)) = (p.as_object_mut(), params.as_object()) {
        p.extend(extra.clone());
    }
    s.execute("document.open", &p)
}

/// The `.ai` private data of `data`, compressed as older `.ai` files compress it.
fn compressed(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    [&b"%AI12_CompressedData"[..], &e.finish().unwrap()].concat()
}

fn names(nodes: &[Arc<Node>]) -> Vec<String> {
    nodes.iter().map(|n| n.name.clone().unwrap_or_default()).collect()
}

/// The sample's structure, as [`ai::sample_data`] builds it.
fn check_sample(s: &Session) {
    let doc = &s.doc().unwrap().doc;
    assert_eq!(names(&doc.layers), ["Art", "Hidden"]);
    assert_eq!(doc.artboards.len(), 1);
    assert_eq!(doc.artboards[0].rect, vectorcraft_geom::Rect::new(0.0, 0.0, 200.0, 100.0));
    let art = doc.layers[0].children().unwrap();
    assert_eq!(names(art), ["Pair", "Ring", "Window"]);
    let pair = art[0].children().unwrap();
    assert_eq!(names(pair), ["Red", ""]);
    assert!(pair[0].visible && !pair[1].visible, "the hidden member stays hidden");
    assert!(matches!(art[1].kind, NodeKind::Compound { .. }));
    assert!(matches!(&art[2].kind, NodeKind::Group { clip: true, children } if matches!(children[0].kind, NodeKind::Path { clipping: true, .. })));
    let hidden = &doc.layers[1];
    assert!(!hidden.visible);
    let kids = hidden.children().unwrap();
    assert_eq!(names(kids), ["Shaded", "", "Sub", ""]);
    assert!(matches!(kids[0].appearance.fill().map(|f| &f.paint), Some(vectorcraft_color::Paint::Gradient(_))));
    assert_eq!((kids[1].opacity, kids[1].blend), (0.5, vectorcraft_color::BlendMode::Screen));
    assert!(kids[2].locked && matches!(kids[2].kind, NodeKind::Layer { .. }));
    let NodeKind::Image(im) = &kids[3].kind else { panic!("an image") };
    assert_eq!((im.width, im.height), (2, 1));
}

#[test]
fn an_illustrator_eps_opens_with_its_layers() {
    let mut s = Session::new();
    let r = open(&mut s, "art.eps", &ai::eps(&ai::sample_data(), ai::page_ps()), json!({})).unwrap();
    assert_eq!((&r["format"], &r["warnings"]), (&json!("eps"), &json!([])), "{r}");
    check_sample(&s);
    // Saved as EPS and opened again, it comes back the same.
    let eps = s.execute("document.exportEps", &json!({})).unwrap();
    let bytes = vectorcraft_format::base64_decode(eps["dataBase64"].as_str().unwrap()).unwrap();
    let r = open(&mut s, "again.eps", &bytes, json!({})).unwrap();
    assert_eq!(r["restored"], json!(true), "{r}");
    check_sample(&s);
}

#[test]
fn an_illustrator_ai_file_opens_from_its_editing_data() {
    let mut s = Session::new();
    let file = ai::ai(&compressed(&ai::sample_data()), ai::page_pdf());
    let r = open(&mut s, "art.ai", &file, json!({})).unwrap();
    assert_eq!(r["format"], json!("ai"), "{r}");
    assert!(r["warnings"].as_array().unwrap().iter().all(|w| !w.as_str().unwrap().contains("weren't read")), "{r}");
    check_sample(&s);
    // Pages picked: the PDF's.
    open(&mut s, "art.ai", &file, json!({"pages": "1"})).unwrap();
    assert_ne!(names(&s.doc().unwrap().doc.layers), ["Art", "Hidden"]);
}

/// A 1-bit RGB image opens from the editing data in the colors its PDF part shows.
#[test]
fn a_one_bit_rgb_image_opens_from_the_editing_data_in_its_colors() {
    // 3 × 1: red, green, blue (100 010 001), as hex samples in both parts.
    let raster = "%AI5_File:\n%AI5_BeginRaster\n() 1 XG\n/DeviceRGB XN\n[ 60 0 0 60 10 90 ] 3 1 0 Xh\n\
         [ 60 0 0 60 10 90 ] 0 0 3 1 3 1 1 3 0 0 0 0 4 4 0 0\n%%BeginData: 6\rXI\n8880\n%%EndData\r\nXH\r\n%AI5_EndRaster\r\nN\r\n";
    let data = ai::editing_data(200.0, 100.0, &ai::layer("Art", raster));
    let page = "q 180 0 0 60 10 30 cm BI /W 3 /H 1 /CS /RGB /BPC 1 /F /AHx ID 8880> EI Q";
    let file = ai::ai(&compressed(data.as_bytes()), page);
    let mut s = Session::new();
    let r = open(&mut s, "bits.ai", &file, json!({})).unwrap();
    let doc = &s.doc().unwrap().doc;
    assert_eq!(names(&doc.layers), ["Art"], "{r}");
    let mut found = vec![];
    doc.visit_images(|_, im| {
        let img = image::load_from_memory(&doc.images[&im.key].bytes).unwrap().to_rgb8();
        found.push(img.pixels().map(|p| p.0).collect::<Vec<_>>());
    });
    assert_eq!(found, [[[255, 0, 0], [0, 255, 0], [0, 0, 255]]], "{r}");
}

/// A PDF saved with the editing data opens from it, as a `.ai` does (#868).
#[test]
fn a_pdf_saved_with_the_editing_data_opens_from_it() {
    let mut s = Session::new();
    let file = ai::pdf(&compressed(&ai::sample_data()), ai::page_pdf());
    let r = open(&mut s, "art.pdf", &file, json!({})).unwrap();
    assert_eq!((&r["format"], &r["warnings"]), (&json!("pdf"), &json!([])), "{r}");
    check_sample(&s);
    // Without it: the PDF part.
    let r = open(&mut s, "art.pdf", &file, json!({"editingData": false})).unwrap();
    let layers = &s.doc().unwrap().doc.layers;
    assert_eq!(layers.len(), 1, "{r}");
    assert_ne!(names(layers), ["Art"]);
}

/// Art crossing the page's edge opens whole from a PDF's editing data, without the note that the
/// PDF's art was clipped to the page; without the editing data it is clipped, with the note.
#[test]
fn a_pdfs_art_past_the_page_opens_whole_from_its_editing_data() {
    let data = ai::editing_data(200.0, 100.0, &ai::layer("Art", &format!("0 g\n{}f\n", ai::rect(150.0, 20.0, 250.0, 80.0))));
    let file = ai::pdf(data.as_bytes(), "0 g 150 20 100 60 re f");
    let mut s = Session::new();
    let r = open(&mut s, "past.pdf", &file, json!({})).unwrap();
    assert_eq!(r["warnings"], json!([]), "{r}");
    let doc = &s.doc().unwrap().doc;
    assert_eq!(names(&doc.layers), ["Art"]);
    assert!(doc.art_bounds().is_some_and(|b| b.x1 > 200.5), "{:?}", doc.art_bounds());
    let r = open(&mut s, "past.pdf", &file, json!({"editingData": false})).unwrap();
    assert_eq!(r["warnings"], json!([vectorcraft_pdf::PAST_PAGE_NOTE]), "{r}");
    assert!(s.doc().unwrap().doc.art_bounds().is_some_and(|b| b.x1 < 200.5));
}

/// Editing data that can't be read leaves the file as its PDF part reads, with that part's notes:
/// a `.ai`'s art outside its artboards is missing, and a PDF's art past the page is clipped.
#[test]
fn editing_data_that_cant_be_read_keeps_the_pdf_parts_notes() {
    let damaged = b"%AI12_CompressedData not zlib";
    let mut s = Session::new();
    let r = open(&mut s, "damaged.ai", &ai::ai(damaged, ai::page_pdf()), json!({})).unwrap();
    assert_eq!(r["warnings"], json!([vectorcraft_pdf::OFF_ARTBOARD_NOTE]), "{r}");
    let r = open(&mut s, "damaged.pdf", &ai::pdf(damaged, "0 g 150 20 100 60 re f"), json!({})).unwrap();
    assert_eq!(r["warnings"], json!([vectorcraft_pdf::PAST_PAGE_NOTE]), "{r}");
}

#[test]
fn what_the_reader_doesnt_read_opens_as_before_with_a_warning() {
    let symbols = ai::editing_data(200.0, 100.0, &ai::layer("Art", "/SymbolInstance :\n(Dot) /SymbolRef ,\n;\n"));
    let mut s = Session::new();
    let r = open(&mut s, "symbols.eps", &ai::eps(symbols.as_bytes(), ai::page_ps()), json!({})).unwrap();
    assert!(r["warnings"].to_string().contains("layers weren't read") && r["warnings"].to_string().contains("symbols"), "{r}");
    assert_eq!(names(&s.doc().unwrap().doc.layers), ["Layer 1"]);
    let r = open(&mut s, "symbols.ai", &ai::ai(&compressed(symbols.as_bytes()), ai::page_pdf()), json!({})).unwrap();
    assert!(r["warnings"].to_string().contains("symbols") && r["warnings"].to_string().contains("PDF part"), "{r}");
}

/// A page showing only a placeholder line, as a `.ai` saved without PDF compatibility has.
const PLACEHOLDER: &str = "BT /F1 12 Tf 10 50 Td (Saved without its PDF part) Tj ET";

#[test]
fn an_ai_file_saved_without_its_pdf_part_opens_from_its_editing_data() {
    let mut s = Session::new();
    let r = open(&mut s, "art.ai", &ai::ai(&compressed(&ai::sample_data()), PLACEHOLDER), json!({})).unwrap();
    assert_eq!(r["format"], json!("ai"), "{r}");
    check_sample(&s);
    // Its editing data damaged: it says so (the placeholder isn't the art).
    let e = open(&mut s, "damaged.ai", &ai::ai(b"%AI12_CompressedData not zlib", PLACEHOLDER), json!({})).unwrap_err().to_string();
    assert!(e.contains("without PDF compatibility") && e.contains("zlib"), "{e}");
    // Without editing data, the placeholder still isn't opened as the art.
    let e = open(&mut s, "plain.ai", &ai::ai(b"", PLACEHOLDER), json!({})).unwrap_err().to_string();
    assert!(e.contains("placeholder"), "{e}");
}

/// A page of text alone, as text-only art has in its PDF part: no path is painted.
const TEXT_ART: &str = "BT /F1 36 Tf 10 40 Td (Text only) Tj ET";

/// The kinds of what the first layer holds: type (or its outlines, without the font) and others.
fn text_and_others(s: &Session) -> (usize, usize) {
    let art = s.doc().unwrap().doc.layers[0].children().unwrap();
    let text = art.iter().filter(|n| matches!(n.kind, NodeKind::Text(_)) || n.name.as_deref() == Some("<Text Outlines>")).count();
    (text, art.len() - text)
}

#[test]
fn an_ai_file_whose_pdf_part_is_only_text_opens_from_it_when_its_editing_data_cant_be_read() {
    // #1387. The page states where its art is (an art, trim or bleed box), which the notice
    // page of a file saved without its PDF part has no art to do.
    let unreadable = b"%AI24_ZStandard_Data\0\0\0\0\0\0\0\0";
    for boxed in ["/ArtBox [0 0 200 100]", "/TrimBox [0 0 200 100]", "/BleedBox [0 0 200 100]"] {
        let mut s = Session::new();
        let r = open(&mut s, "text.ai", &ai::ai_with(unreadable, TEXT_ART, boxed), json!({})).unwrap();
        // Then a note on the font, where the machine lacks it.
        assert_eq!(r["warnings"][0], json!(vectorcraft_pdf::OFF_ARTBOARD_NOTE), "{boxed}: {r}");
        assert_eq!(text_and_others(&s), (1, 0), "{boxed}");
        // The PDF part alone, as asked.
        open(&mut s, "text.ai", &ai::ai_with(unreadable, TEXT_ART, boxed), json!({"editingData": false})).unwrap();
        assert_eq!(text_and_others(&s), (1, 0), "{boxed}");
    }
    // The same page without a box is the notice: the file still says why it can't open.
    let mut s = Session::new();
    let e = open(&mut s, "text.ai", &ai::ai(unreadable, TEXT_ART), json!({})).unwrap_err().to_string();
    assert!(e.contains("without PDF compatibility") && e.contains("can't be read"), "{e}");
    let e = open(&mut s, "text.ai", &ai::ai(unreadable, TEXT_ART), json!({"editingData": false})).unwrap_err().to_string();
    assert!(e.contains("placeholder"), "{e}");
    // Placed, it is read as its page too.
    let r = s.execute("file.new", &json!({})).and_then(|_| {
        let data = vectorcraft_format::base64_encode(&ai::ai_with(unreadable, TEXT_ART, "/ArtBox [0 0 200 100]"));
        s.execute("file.place", &json!({"name": "text.ai", "dataBase64": data, "link": false}))
    });
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn an_ai_file_whose_pdf_part_is_only_text_still_opens_from_editing_data_that_reads() {
    let mut s = Session::new();
    open(&mut s, "art.ai", &ai::ai_with(&compressed(&ai::sample_data()), TEXT_ART, "/ArtBox [0 0 200 100]"), json!({})).unwrap();
    check_sample(&s);
}

#[test]
fn editing_data_false_opens_only_what_the_page_prints() {
    let mut s = Session::new();
    for (name, file) in [("art.eps", ai::eps(&ai::sample_data(), ai::page_ps())), ("art.ai", ai::ai(&compressed(&ai::sample_data()), ai::page_pdf()))]
    {
        let r = open(&mut s, name, &file, json!({"editingData": false})).unwrap();
        let layers = &s.doc().unwrap().doc.layers;
        assert_eq!(layers.len(), 1, "{name}: {r}");
        assert_ne!(names(layers), ["Art"], "{name}: the page's layer");
    }
    let e = open(&mut s, "art.eps", &ai::eps(&ai::sample_data(), ai::page_ps()), json!({"editingData": "no"})).unwrap_err().to_string();
    assert!(e.contains("editingData must be true, false or \"always\""), "{e}");
    // `always`: the editing data, as by default (whether it draws as the page does is the reader's).
    open(&mut s, "art.ai", &ai::ai(&compressed(&ai::sample_data()), ai::page_pdf()), json!({"editingData": "always"})).unwrap();
    assert_eq!(names(&s.doc().unwrap().doc.layers), ["Art", "Hidden"]);
    // A file that is only its editing data can't open without it.
    let e = open(&mut s, "art.ai", &ai::ai(&compressed(&ai::sample_data()), PLACEHOLDER), json!({"editingData": false})).unwrap_err().to_string();
    assert!(e.contains("placeholder"), "{e}");
}

/// A CMYK image opens with the same ink amounts from a `.ai` file's editing data as from its PDF
/// part.
#[test]
fn a_cmyk_image_keeps_its_inks_from_the_editing_data_as_from_the_pdf_part() {
    // 3 × 1: cyan, 100% black, then a mix, as hex samples in both parts.
    let inks = [255u8, 0, 0, 0, 0, 0, 0, 255, 12, 34, 56, 78];
    let hex: String = inks.iter().map(|b| format!("{b:02x}")).collect();
    let raster = format!(
        "%AI5_File:\n%AI5_BeginRaster\n() 1 XG\n/DeviceCMYK XN\n[ 10 0 0 10 10 60 ] 3 1 0 Xh\n\
         [ 10 0 0 10 10 60 ] 0 0 3 1 3 1 8 4 0 0 0 0 4 4 0 0\n%%BeginData: {}\rXI\n{hex}\n%%EndData\r\nXH\r\n%AI5_EndRaster\r\nN\r\n",
        hex.len() + 2
    );
    let data = ai::editing_data(200.0, 100.0, &ai::layer("Art", &raster));
    let page = format!("q 30 0 0 10 10 50 cm BI /W 3 /H 1 /CS /CMYK /BPC 8 /F /AHx ID {hex}> EI Q");
    let file = ai::ai(&compressed(data.as_bytes()), &page);
    let opened = |s: &Session| {
        let doc = &s.doc().unwrap().doc;
        let mut found = vec![];
        doc.visit_images(|_, im| found.push(doc.images[&im.key].cmyk().map(|i| i.data)));
        found
    };
    let mut s = Session::new();
    let r = open(&mut s, "cmyk.ai", &file, json!({})).unwrap();
    assert_eq!(names(&s.doc().unwrap().doc.layers), ["Art"], "{r}");
    assert_eq!(opened(&s), [Some(inks.to_vec())], "from the editing data");
    let r = open(&mut s, "cmyk.ai", &file, json!({"editingData": false})).unwrap();
    assert_ne!(names(&s.doc().unwrap().doc.layers), ["Art"], "{r}");
    assert_eq!(opened(&s), [Some(inks.to_vec())], "from the PDF part");
}

#[test]
fn fit_to_artwork_bounds_leaves_out_the_hidden_art_and_guides_the_editing_data_has() {
    // A shown square; a hidden one and a long guide far from it, which the page doesn't draw.
    let art =
        format!("0 g\n{}f\n1 Xw\n{}f\n0 Xw\n-500 20 m\n700 20 L\n(N) *\n", ai::rect(10.0, 10.0, 50.0, 50.0), ai::rect(150.0, 60.0, 190.0, 95.0));
    let data = ai::editing_data(200.0, 100.0, &ai::layer("Art", &art));
    let mut s = Session::new();
    let r = open(&mut s, "fit.eps", &ai::eps(data.as_bytes(), "0 setgray 10 10 40 40 rectfill\n"), json!({})).unwrap();
    let layer = &s.doc().unwrap().doc.layers[0];
    let kids = layer.children().unwrap();
    assert_eq!(kids.len(), 3, "{r}");
    assert!(!kids[1].visible && matches!(kids[2].kind, NodeKind::Path { guide: true, .. }));
    s.execute("artboard.fitToArt", &json!({})).unwrap();
    // The square, 50 to 90 down the 100-point page.
    assert_eq!(s.doc().unwrap().doc.artboards[0].rect, vectorcraft_geom::Rect::new(10.0, 50.0, 50.0, 90.0));
}

#[test]
fn type_asked_for_as_outlines_opens_a_file_whose_type_shows_from_its_pdf_part() {
    let text = "/AI11Text :\n0 /FreeUndo ,\n0 /FrameIndex ,\n0 /StoryIndex ,\n;\n";
    let data = ai::editing_data(200.0, 100.0, &ai::layer("Art", &format!("0 g\n{}f\n{text}", ai::rect(10.0, 10.0, 50.0, 50.0))));
    let file = ai::ai(&compressed(data.as_bytes()), "0 g 10 10 40 40 re f BT /F1 12 Tf 100 50 Td (Hi) Tj ET");
    let mut s = Session::new();
    // As type, the page's type goes into the text object of the layers.
    let r = open(&mut s, "type.ai", &file, json!({})).unwrap();
    assert_eq!(names(&s.doc().unwrap().doc.layers), ["Art"], "{r}");
    // As outlines, only the PDF part has them.
    let r = open(&mut s, "type.ai", &file, json!({"textAs": "outlines"})).unwrap();
    assert!(r["warnings"].to_string().contains("outlines"), "{r}");
    assert_ne!(names(&s.doc().unwrap().doc.layers), ["Art"], "{r}");
}

/// A `.ai` saved in units of 10 points: its PDF part's page is 200 × 100 with `/UserUnit 10`, and
/// its editing data has the artboard and a red rectangle in the same units. It opens at 2000 × 1000
/// pt, from its editing data and from its PDF part alone, with the rectangle ten times as large,
/// and `document.pdfInfo` gives the page in points (#1482).
#[test]
fn an_ai_file_with_a_user_unit_opens_at_its_size_in_points() {
    let data = ai::editing_data(200.0, 100.0, &ai::layer("Art", &format!("0 1 1 0 1 0 0 Xa\n{}f\n", ai::rect(20.0, 20.0, 60.0, 50.0))));
    let file = ai::ai_with(&compressed(data.as_bytes()), "1 0 0 rg 20 20 40 30 re f", "/UserUnit 10");
    let near = |r: vectorcraft_geom::Rect, want: [f64; 4]| [r.x0, r.y0, r.x1, r.y1].iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01);
    let mut s = Session::new();
    for (params, layers) in [(json!({}), ["Art"]), (json!({"editingData": false}), ["Page 1"])] {
        let r = open(&mut s, "large.ai", &file, params.clone()).unwrap();
        let doc = &s.doc().unwrap().doc;
        assert_eq!(names(&doc.layers), layers, "{params}: {r}");
        assert!(near(doc.artboards[0].rect, [0.0, 0.0, 2000.0, 1000.0]), "{params}: {:?}", doc.artboards[0].rect);
        let art = doc.art_bounds().unwrap();
        assert!(near(art, [200.0, 500.0, 600.0, 800.0]), "{params}: {art:?}");
    }
    let v = s.execute("document.pdfInfo", &json!({"dataBase64": vectorcraft_format::base64_encode(&file)})).unwrap();
    let page = &v["pageInfo"][0];
    assert_eq!((page["width"].as_f64(), page["height"].as_f64()), (Some(2000.0), Some(1000.0)), "{v}");
    assert_eq!(page["boxes"]["media"], json!([0.0, 0.0, 2000.0, 1000.0]), "{v}");
}

/// A `.ai` of a 200 × 100 pt artboard with a 10 mm bleed: its PDF part's page is the artboard
/// (its trim box) grown by the bleed, and a red square reaches 20 pt into the bleed, on the page
/// as in the editing data. `key`: `AIPrivateData`, or `AIPDFPrivateData` for a PDF saved with its
/// editing data.
fn bled_ai(bleed: f64, key: &str) -> Vec<u8> {
    let art = format!("0 1 1 0 1 0 0 Xa\n{}f\n", ai::rect(-20.0, -20.0, 60.0, 50.0));
    let data = ai::editing_data(200.0, 100.0, &ai::layer("Art", &art));
    let (w, h) = (200.0 + 2.0 * bleed, 100.0 + 2.0 * bleed);
    let page = PdfPage {
        bleed: Some([0.0, 0.0, w, h]),
        trim: Some([bleed, bleed, 200.0 + bleed, 100.0 + bleed]),
        entries: format!("/PieceInfo << /Illustrator << /Private << /{key}1 {} 0 R /NumBlock 1 >> >> >>", first_extra(1)),
        ..PdfPage::new(w, h, &format!("1 0 0 rg {0} {0} 80 70 re f", bleed - 20.0))
    };
    pdf_with(&[page], &[&format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())], None)
}

#[test]
fn a_file_with_a_bleed_opens_on_its_artboard_with_the_bleed_round_it() {
    // A `.ai`, and a PDF saved with its editing data (#1345), which has it under other keys.
    for (name, key) in [("bled.ai", "AIPrivateData"), ("bled.pdf", "AIPDFPrivateData")] {
        bled_file_opens_on_its_artboard(name, key);
    }
}

fn bled_file_opens_on_its_artboard(name: &str, key: &str) {
    let bleed = 10.0 * 72.0 / 25.4;
    let file = bled_ai(bleed, key);
    let mut s = Session::new();
    // From the editing data, and from the PDF part alone.
    for params in [json!({}), json!({"editingData": false})] {
        let r = open(&mut s, name, &file, params.clone()).unwrap();
        let doc = &s.doc().unwrap().doc;
        assert_eq!(doc.artboards.len(), 1, "{name} {params}: {r}");
        let ab = doc.artboards[0].rect;
        assert!((ab.width() - 200.0).abs() < 0.01 && (ab.height() - 100.0).abs() < 0.01, "{name} {params}: {ab:?} {r}");
        assert!(doc.setup.bleed.iter().all(|b| (b - bleed).abs() < 0.01), "{name} {params}: {:?}", doc.setup.bleed);
        // The square reaches into the bleed, whole.
        let art = doc.art_bounds().unwrap();
        let want = [ab.x0 - 20.0, ab.y0 + 50.0, ab.x0 + 60.0, ab.y0 + 120.0];
        assert!([art.x0, art.y0, art.x1, art.y1].iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01), "{name} {params}: {art:?}");
        assert!(doc.setup.bleed_rect(ab).inflate(0.01, 0.01).contains_rect(art), "{name} {params}: {art:?}");
    }
    assert_eq!(names(&s.doc().unwrap().doc.layers), ["Page 1"], "{name}: editingData: false reads the PDF part");
    open(&mut s, name, &file, json!({})).unwrap();
    assert_eq!(names(&s.doc().unwrap().doc.layers), ["Art"], "{name}: the editing data's layers are read");
}

/// Original synthetic PDF: a CMYK image and soft mask, plus an RGB editing
/// preview. Looking alike on screen must not allow that preview to replace inks.
#[test]
fn ordinary_pdf_open_keeps_masked_cmyk_instead_of_rgb_editing_preview() {
    // Small art passes the existing page-wide visual tolerance despite different inks.
    let hex = "000000000000ff80"; // two RGB black pixels, then alpha
    let raster = format!(
        "%AI5_File:\n%AI5_BeginRaster\n() 1 XG\n/DeviceRGB XN\n[ 2 0 0 2 10 60 ] 2 1 0 Xh\n\
         [ 2 0 0 2 10 60 ] 0 0 2 1 2 1 8 3 1 0 0 0 4 4 0 0\n%%BeginData: {}\rXI\n{hex}\n%%EndData\r\nXH\r\n%AI5_EndRaster\r\nN\r\n",
        hex.len() + 2
    );
    let private = ai::editing_data(200.0, 100.0, &ai::layer("RGB preview", &raster));
    let stream = |dict: &str, text: &str| format!("<< {dict} /Length {} >>\nstream\n{text}\nendstream", text.len());
    for mask_kind in ["image", "Luminosity", "Alpha"] {
        let mut page = PdfPage::new(200.0, 100.0, "q 4 0 0 2 10 58 cm /Im Do Q");
        page.resources = "/XObject << /Im 5 0 R >> /ExtGState << /GS << /SMask << /S /Luminosity /G 8 0 R >> >> >>".into();
        if mask_kind != "image" {
            page.content = "q /GS gs 4 0 0 2 10 58 cm /Im Do Q".into();
            page.resources = page.resources.replace("/Luminosity", &format!("/{mask_kind}"));
        }
        // The private-data dictionary name is part of the documented file format.
        let piece = "Illustrator"; // brand-ok: PDF dictionary key
        page.entries = format!("/PieceInfo << /{piece} << /Private << /AIPDFPrivateData1 7 0 R /NumBlock 1 >> >> >>");
        let image = stream(
            &format!(
                "/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceCMYK /BitsPerComponent 8 /Filter /ASCIIHexDecode {}",
                if mask_kind == "image" { "/SMask 6 0 R" } else { "" }
            ),
            "000000ff000000ff>",
        );
        let mask =
            stream("/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /ASCIIHexDecode", "ff80>");
        let data = stream("", &private);
        let form = stream(
            "/Type /XObject /Subtype /Form /BBox [10 58 14 60] /Group << /S /Transparency /CS /DeviceGray >> /Resources << /ExtGState << /Half << /ca 0.5 >> >> >>",
            if mask_kind == "Alpha" { "1 g 10 58 2 2 re f /Half gs 12 58 2 2 re f" } else { "1 g 10 58 2 2 re f 0.5 g 12 58 2 2 re f" },
        );
        let file = pdf_with(&[page], &[&image, &mask, &data, &form], None);
        let mut s = Session::new();
        let result = open(&mut s, "masked.pdf", &file, json!({})).unwrap();
        if mask_kind == "image" {
            assert!(
                result["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.contains("changes CMYK image inks"))),
                "fixture must exercise the ink guard: {result}"
            );
        }
        let doc = &s.doc().unwrap().doc;
        let mut inks = vec![];
        let mut masks = 0;
        doc.walk(|n| masks += usize::from(n.extras().mask.is_some()));
        doc.visit_images(|_, im| {
            if let Some(cmyk) = doc.images[&im.key].cmyk() {
                inks.push(cmyk.data);
            }
        });
        assert_eq!(inks, [vec![0, 0, 0, 255, 0, 0, 0, 255]], "{mask_kind}: {result}");
        assert!(masks > 0, "{mask_kind}: mask retained");
        // Force the alternative route to prove this fixture exercises substitution.
        let mut trusted = Session::new();
        open(&mut trusted, "masked.pdf", &file, json!({"editingData": "always"})).unwrap();
        assert!(trusted.doc().unwrap().doc.images.values().all(|b| !b.is_cmyk()));
        let out = s.execute("document.exportPdf", &json!({"preserveEditing": false})).unwrap();
        let bytes = vectorcraft_format::base64_decode(out["dataBase64"].as_str().unwrap()).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("/DeviceCMYK"), "{mask_kind}: export keeps DeviceCMYK");
        let exported = vectorcraft_pdf::import(&bytes).unwrap();
        assert!(exported.images.values().any(|b| b.cmyk().is_some_and(|i| i.data == inks[0])));
    }
}

fn cmyk_guard_raster(space: &str, hex: &str, components: u8, w: u32, h: u32) -> String {
    format!(
        "%AI5_File:\n%AI5_BeginRaster\n() 1 XG\n/{space} XN\n[ 2 0 0 2 10 60 ] {w} {h} 0 Xh\n\
         [ 2 0 0 2 10 60 ] 0 0 {w} {h} {w} {h} 8 {components} 0 0 0 0 4 4 0 0\n\
         %%BeginData: {}\rXI\n{hex}\n%%EndData\r\nXH\r\n%AI5_EndRaster\r\nN\r\n",
        hex.len() + 2
    )
}

fn cmyk_guard_pdf(body: &str, samples: &str, filter: &str, w: u32, h: u32) -> Vec<u8> {
    let private = ai::editing_data(200.0, 100.0, body);
    let stream = |dict: &str, text: &str| format!("<< {dict} /Length {} >>\nstream\n{text}\nendstream", text.len());
    let image =
        stream(&format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceCMYK /BitsPerComponent 8 {filter}"), samples);
    let mut page = PdfPage::new(200.0, 100.0, &format!("q {} 0 0 {} 10 {} cm /Im Do Q", 2 * w, 2 * h, 60 - 2 * h));
    page.resources = "/XObject << /Im 5 0 R >>".into();
    page.entries = "/PieceInfo << /Illustrator << /Private << /AIPDFPrivateData1 6 0 R /NumBlock 1 >> >> >>".into();
    pdf_with(&[page], &[&image, &stream("", &private)], None)
}

#[test]
fn a_hidden_cmyk_copy_does_not_authorize_a_visible_rgb_replacement() {
    let hex = "000000ff000000ff";
    let rgb = ai::layer("Visible RGB", &cmyk_guard_raster("DeviceRGB", "000000000000", 3, 2, 1));
    let hidden = ai::layer("Hidden CMYK", &cmyk_guard_raster("DeviceCMYK", hex, 4, 2, 1)).replacen("1 1 1 1 0 0 1", "0 1 1 1 0 0 1", 1);
    let file = cmyk_guard_pdf(&(rgb + &hidden), &format!("{hex}>"), "/Filter /ASCIIHexDecode", 2, 1);
    let mut s = Session::new();
    let r = open(&mut s, "hidden.pdf", &file, json!({})).unwrap();
    assert_eq!(names(&s.doc().unwrap().doc.layers), ["Page 1"], "{r}");
    assert!(r["warnings"].to_string().contains("CMYK image inks"), "{r}");
    let exported = s.execute("document.exportPdf", &json!({"preserveEditing": false})).unwrap();
    let bytes = vectorcraft_format::base64_decode(exported["dataBase64"].as_str().unwrap()).unwrap();
    open(&mut s, "saved.pdf", &bytes, json!({"editingData": false})).unwrap();
    let doc = &s.doc().unwrap().doc;
    let mut images = 0;
    doc.visit_images(|_, im| {
        images += 1;
        assert_eq!(doc.images[&im.key].cmyk().unwrap().data, [0, 0, 0, 255, 0, 0, 0, 255]);
    });
    assert_eq!(images, 1);
}

#[test]
fn jpeg_public_inks_keep_editable_layers_and_hidden_originals() {
    let inks: Vec<u8> = (0..16 * 16).flat_map(|i| [((i * 7) % 256) as u8, 40, 100, 20]).collect();
    let mut jpg = vec![];
    jpeg_encoder::Encoder::new(&mut jpg, 90).encode(&inks, 16, 16, jpeg_encoder::ColorType::Cmyk).unwrap();
    let hex = |data: &[u8]| data.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let raster = cmyk_guard_raster("DeviceCMYK", &hex(&inks), 4, 16, 16);
    let hidden = ai::layer("Hidden original", &raster).replacen("1 1 1 1 0 0 1", "0 1 1 1 0 0 1", 1);
    let body = ai::layer("Editable art", &raster) + &hidden;
    let file = cmyk_guard_pdf(&body, &format!("{}>", hex(&jpg)), "/Filter [/ASCIIHexDecode /DCTDecode] /Decode [1 0 1 0 1 0 1 0]", 16, 16);
    let page = load_with("page.pdf", &file, &LoadOptions { editing_data: false, ..Default::default() }).unwrap();
    let mut expected = vec![];
    page.doc.visit_images(|_, im| expected.push(page.doc.images[&im.key].cmyk().unwrap().data));
    assert_ne!(expected[0], inks, "the fixture must exercise lossy compression");
    let mut s = Session::new();
    let r = open(&mut s, "jpeg.pdf", &file, json!({})).unwrap();
    let doc = &s.doc().unwrap().doc;
    assert_eq!(names(&doc.layers), ["Editable art", "Hidden original"], "{r}");
    assert!(!doc.layers[1].visible);
    let mut actual = vec![];
    doc.visit_images(|_, im| actual.push(doc.images[&im.key].cmyk().unwrap().data));
    assert_eq!(actual, [expected[0].clone(), inks], "only the printed occurrence uses public pixels");
    let exported = s.execute("document.exportPdf", &json!({"preserveEditing": false})).unwrap();
    let bytes = vectorcraft_format::base64_decode(exported["dataBase64"].as_str().unwrap()).unwrap();
    open(&mut s, "saved.pdf", &bytes, json!({"editingData": false})).unwrap();
    let doc = &s.doc().unwrap().doc;
    let mut actual = vec![];
    doc.visit_images(|_, im| actual.push(doc.images[&im.key].cmyk().unwrap().data));
    assert_eq!(actual, expected, "export/reopen keeps public process samples");
}
