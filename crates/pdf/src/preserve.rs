//! Retain the public PDF print settings independently of editable artwork. No
//! source content streams or application editing data are retained here.
use std::collections::{BTreeMap, BTreeSet};

use craft_pdf::syntax::Pdf;
use craft_pdf::syntax::object::{Array, Dict, Name, Stream};
use serde::{Deserialize, Serialize};
use vectorcraft_doc::Document;

use crate::encrypt::{Lexer, Obj, Tok};
use crate::patch::Patch;
use crate::{ColorConversion, CropTo, PdfError, PdfOptions, Standard};

const KEY: &str = "pdf.printSettings.v1";
const MAX_PROFILE: usize = 16 << 20;
const BOXES: [&str; 5] = ["MediaBox", "CropBox", "TrimBox", "BleedBox", "ArtBox"];

#[derive(Default, Serialize, Deserialize)]
struct State {
    pages: Vec<Page>,
    intents: Vec<Intent>,
}
#[derive(Serialize, Deserialize)]
struct Page {
    artboard: u32,
    frame: [f64; 4],
    origin: [f64; 2],
    boxes: BTreeMap<String, [f64; 4]>,
    group: Option<Group>,
}
#[derive(Serialize, Deserialize)]
struct Group {
    space: String,
    isolate: bool,
    knockout: bool,
}
#[derive(Serialize, Deserialize)]
struct Intent {
    kind: String,
    text: BTreeMap<String, String>,
    profile: Vec<u8>,
    components: u8,
}

fn error(msg: &str) -> PdfError {
    PdfError::Write(format!("can't preserve imported PDF print settings: {msg}"))
}
fn rect(r: vectorcraft_geom::Rect) -> [f64; 4] {
    [r.x0, r.y0, r.x1, r.y1]
}

/// Store recognized, bounded public print settings with the native document.
pub(crate) fn capture(doc: &mut Document, pdf: &Pdf, selected: &[usize], crop: CropTo, warnings: &mut Vec<String>) {
    let mut state = State::default();
    for (i, &number) in selected.iter().enumerate() {
        let (Some(page), Some(board)) = (pdf.pages().get(number), doc.artboards.get(i)) else { continue };
        if !matches!(page.rotation(), craft_pdf::syntax::page::Rotation::None) || page.user_unit() != 1.0 || crop == CropTo::Bounding {
            warnings.push("original PDF page boxes are not retained for a rotated, scaled-unit or artwork-bounds import".into());
            continue;
        }
        let mut boxes = BTreeMap::new();
        for name in BOXES {
            if let Some(v) = page.raw().get::<[f64; 4]>(name.as_bytes()).filter(valid_box) {
                boxes.insert(name.into(), v);
            }
        }
        boxes.entry("MediaBox".into()).or_insert_with(|| rect(crate::pages::page_box(page, CropTo::Media)));
        boxes.entry("CropBox".into()).or_insert_with(|| rect(crate::pages::page_box(page, CropTo::Crop)));
        let group = page.raw().get::<Dict<'_>>(b"Group").and_then(|g| {
            let Some(name) = g.get::<Name<'_>>(b"CS") else {
                warnings.push("the imported page transparency group has an unsupported blending space".into());
                return None;
            };
            let space = name.as_str();
            if !matches!(space, "DeviceRGB" | "DeviceCMYK" | "DeviceGray") {
                warnings.push("the imported page transparency group's blending space cannot be retained".into());
                return None;
            }
            Some(Group { space: space.into(), isolate: g.get::<bool>(b"I").unwrap_or(false), knockout: g.get::<bool>(b"K").unwrap_or(false) })
        });
        let source = crate::pages::page_box(page, crop);
        state.pages.push(Page { artboard: board.id, frame: rect(board.rect), origin: [source.x0, source.y0], boxes, group });
    }
    if let Some(catalog) = pdf.xref().get::<Dict<'_>>(pdf.xref().root_id())
        && let Some(intents) = catalog.get::<Array<'_>>(b"OutputIntents")
    {
        for d in intents.iter::<Dict<'_>>().take(8) {
            let kind = d.get::<Name<'_>>(b"S").map(|n| n.as_str().to_string()).unwrap_or_default();
            if !matches!(kind.as_str(), "GTS_PDFX" | "GTS_PDFA1" | "ISO_PDFE1") {
                continue;
            }
            let mut text = BTreeMap::new();
            for name in ["OutputConditionIdentifier", "OutputCondition", "RegistryName", "Info"] {
                if let Some(v) = d.get::<craft_pdf::syntax::object::String<'_>>(name.as_bytes()) {
                    let value = crate::import_scan::text_string(v.as_bytes());
                    if value.len() <= 65536 {
                        text.insert(name.into(), value);
                    }
                }
            }
            let mut profile = vec![];
            let mut components = 0;
            if let Some(st) = d.get::<Stream<'_>>(b"DestOutputProfile") {
                components = st.dict().get::<u8>(b"N").unwrap_or(0);
                match st.decoded_within(MAX_PROFILE) {
                    Ok(data) if matches!(components, 1 | 3 | 4) => profile = data.into_owned(),
                    _ => {
                        warnings.push("an output intent's ICC profile could not be retained".into());
                        continue;
                    }
                }
            }
            state.intents.push(Intent { kind, text, profile, components });
        }
    }
    match serde_json::to_value(&state) {
        Ok(value) => {
            doc.unknown.insert(KEY.into(), value);
        }
        Err(_) => warnings.push("the imported PDF print settings could not be retained".into()),
    }
}

fn valid_box(v: &[f64; 4]) -> bool {
    v.iter().all(|v| v.is_finite() && v.abs() < 1e8) && v[2] > v[0] && v[3] > v[1]
}

/// Replace dictionary keys without duplicate keys or copying executable PDF data
/// from the source file. The dictionary being edited is our own writer's output.
fn dictionary(pdf: &[u8], obj: &Obj<'_>, replacements: &BTreeMap<&str, String>) -> Result<Vec<u8>, PdfError> {
    let Obj::Dict { start, end, .. } = obj else { return Err(error("expected a dictionary")) };
    let mut lx = Lexer::at(pdf, start + 2);
    let mut out = b"<<".to_vec();
    while let Some((token, begin, _)) = lx.next().filter(|t| t.1 < *end - 2) {
        let Tok::Name(name) = token else { return Err(error("bad dictionary key")) };
        lx.object(0).ok_or_else(|| error("bad dictionary value"))?;
        if !replacements.keys().any(|k| k.as_bytes() == name) {
            out.push(b' ');
            out.extend_from_slice(pdf.get(begin..lx.position()).ok_or_else(|| error("bad dictionary span"))?);
        }
    }
    for (key, value) in replacements {
        out.extend_from_slice(format!(" /{key} {value}").as_bytes());
    }
    out.extend_from_slice(b" >>");
    Ok(out)
}
fn stream(data: &[u8]) -> Vec<u8> {
    let mut out = format!("<</Length {}>>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// Groups introduced by the writer inherit the imported page's blending space.
/// A CMYK working document does not imply that the source PDF blended in CMYK.
/// Follow painted XObjects only: soft-mask groups have their own luminance space.
fn inherited_groups(parsed: &crate::linearize::Parsed<'_>, page: u32, space: &str, groups: &mut BTreeMap<u32, String>) -> Result<(), PdfError> {
    fn resolve<'a>(parsed: &'a crate::linearize::Parsed<'a>, value: &'a Obj<'a>) -> Option<&'a Obj<'a>> {
        match value {
            Obj::Ref(n, _) => parsed.objects.get(n).map(|o| &o.value),
            v => Some(v),
        }
    }
    let mut seen = BTreeSet::new();
    let mut pending = vec![page];
    while let Some(number) = pending.pop() {
        if !seen.insert(number) {
            continue;
        }
        let Some(obj) = parsed.objects.get(&number) else { continue };
        if number != page && obj.value.get(b"Group").is_some() {
            if groups.get(&number).is_some_and(|old| old != space) {
                return Err(error("a transparency form is shared by pages with different blending spaces"));
            }
            groups.insert(number, space.into());
        }
        let Some(resources) = obj.value.get(b"Resources").and_then(|v| resolve(parsed, v)) else { continue };
        let Some(Obj::Dict { entries, .. }) = resources.get(b"XObject").and_then(|v| resolve(parsed, v)) else { continue };
        for (_, value) in entries {
            if let Obj::Ref(n, _) = value
                && parsed.objects.get(n).is_some_and(|o| o.value.name(b"Subtype") == Some(b"Form"))
            {
                pending.push(*n);
            }
        }
    }
    Ok(())
}

pub(crate) fn finish(pdf: Vec<u8>, doc: &Document, opts: &PdfOptions, units: &[f64], warnings: &mut Vec<String>) -> Result<Vec<u8>, PdfError> {
    let Some(value) = doc.unknown.get(KEY) else { return Ok(pdf) };
    if opts.settings.discard_imported_print_settings {
        return Ok(pdf);
    }
    let state: State = serde_json::from_value(value.clone()).map_err(|_| error("invalid saved print settings"))?;
    if opts.settings.standard != Standard::None
        || opts.settings.pdf13()
        || !matches!(opts.settings.output.conversion, ColorConversion::None | ColorConversion::PreserveSource)
    {
        warnings.push("imported PDF print settings were not restored because the export requests a standard, flattening or colour conversion".into());
        return Ok(pdf);
    }
    let parsed = crate::linearize::parse(&pdf).map_err(error)?;
    if units.len() != parsed.pages.len() || units.iter().any(|u| !u.is_finite() || *u < 1.0) {
        return Err(error("invalid output page units"));
    }
    let mut patch = Patch::new(&parsed.xref);
    let mut inherited = BTreeMap::new();
    let indices: Vec<usize> = opts.artboards.clone().unwrap_or_else(|| (0..doc.artboards.len()).collect());
    for ((&number, &index), &unit) in parsed.pages.iter().zip(&indices).zip(units) {
        let Some(board) = doc.artboards.get(index) else { continue };
        let Some(source) = state.pages.iter().find(|s| s.artboard == board.id) else { continue };
        let keep_boxes = !(source.frame != rect(board.rect)
            || opts.settings.marks != Default::default()
            || opts.settings.bleed != Default::default()
            || doc.setup.bleed.iter().any(|&b| b != 0.0));
        if !keep_boxes {
            warnings.push("original PDF page boxes were not restored because the artboard, bleed or printer marks changed".into());
        }
        if !source.boxes.values().all(valid_box) || source.origin.iter().any(|x| !x.is_finite() || x.abs() >= 1e8) {
            return Err(error("invalid saved page boxes"));
        }
        let obj = parsed.objects.get(&number).ok_or_else(|| error("missing output page"))?;
        let mut replacements = BTreeMap::new();
        for name in BOXES {
            if let Some(v) = source.boxes.get(name).filter(|_| keep_boxes) {
                replacements.insert(name, format!("[{} {} {} {}]", v[0] / unit, v[1] / unit, v[2] / unit, v[3] / unit));
            }
        }
        if let Some(group) = &source.group {
            if !matches!(group.space.as_str(), "DeviceRGB" | "DeviceCMYK" | "DeviceGray") {
                return Err(error("invalid blending space"));
            }
            inherited_groups(&parsed, number, &group.space, &mut inherited)?;
            replacements.insert("Group", format!("<</Type/Group/S/Transparency/CS/{}/I {}/K {}>>", group.space, group.isolate, group.knockout));
        }
        if keep_boxes && source.origin != [0.0, 0.0] {
            let refs = match obj.value.get(b"Contents") {
                Some(Obj::Ref(n, g)) => format!("{n} {g} R"),
                Some(Obj::Array(items)) => items
                    .iter()
                    .map(|v| match v {
                        Obj::Ref(n, g) => Ok(format!("{n} {g} R")),
                        _ => Err(error("non-reference page content")),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join(" "),
                _ => return Err(error("missing page content")),
            };
            let pre = patch.add_object(stream(format!("q 1 0 0 1 {} {} cm\n", source.origin[0] / unit, source.origin[1] / unit).as_bytes()));
            let post = patch.add_object(stream(b"Q\n"));
            replacements.insert("Contents", format!("[{pre} 0 R {refs} {post} 0 R]"));
        }
        let updated = dictionary(&pdf, &obj.value, &replacements)?;
        patch.replace(obj.value_span.0, obj.value_span.1, updated);
    }
    for (number, space) in inherited {
        let obj = parsed.objects.get(&number).ok_or_else(|| error("missing transparency form"))?;
        let group = obj.value.get(b"Group").ok_or_else(|| error("missing transparency group"))?;
        let Obj::Dict { start, end, .. } = group else { return Err(error("indirect transparency group")) };
        patch.replace(*start, *end, dictionary(&pdf, group, &BTreeMap::from([("CS", format!("/{space}"))]))?);
    }
    let output = &opts.settings.output;
    let explicit_intent = !output.output_intent.is_empty() || !output.output_condition_id.is_empty();
    if !state.intents.is_empty() && !explicit_intent {
        if state.intents.len() > 8 {
            return Err(error("too many saved output intents"));
        }
        let mut refs = vec![];
        for intent in &state.intents {
            if !matches!(intent.kind.as_str(), "GTS_PDFX" | "GTS_PDFA1" | "ISO_PDFE1") || intent.profile.len() > MAX_PROFILE {
                return Err(error("invalid output intent"));
            }
            let mut dict = format!("<</Type/OutputIntent/S/{}", intent.kind);
            for key in ["OutputConditionIdentifier", "OutputCondition", "RegistryName", "Info"] {
                if let Some(v) = intent.text.get(key) {
                    dict.push_str(&format!("/{key}{}", crate::output::text_string(v)));
                }
            }
            if !intent.profile.is_empty() {
                if !matches!(intent.components, 1 | 3 | 4) {
                    return Err(error("invalid ICC component count"));
                }
                let mut profile = format!("<</N {}/Length {}>>\nstream\n", intent.components, intent.profile.len()).into_bytes();
                profile.extend_from_slice(&intent.profile);
                profile.extend_from_slice(b"\nendstream");
                dict.push_str(&format!("/DestOutputProfile {} 0 R", patch.add_object(profile)));
            }
            dict.push_str(">>");
            refs.push(format!("{} 0 R", patch.add_object(dict.into_bytes())));
        }
        let catalog = parsed.objects.get(&parsed.root).ok_or_else(|| error("missing catalog"))?;
        let replacements = BTreeMap::from([("OutputIntents", format!("[{}]", refs.join(" ")))]);
        patch.replace(catalog.value_span.0, catalog.value_span.1, dictionary(&pdf, &catalog.value, &replacements)?);
    }
    patch.apply(&pdf, &parsed.xref)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImportOptions, export, import_with_report};
    use vectorcraft_testkit::pdf::{PdfPage, pdf_with};

    fn fixture() -> Vec<u8> {
        // Handwritten public PDF data, no vendor fixture. Negative origin and
        // unequal ArtBox ensure dimension-only preservation cannot pass.
        let mut page = PdfPage::new(100.0, 80.0, "0 0 0 1 k 12 9 30 20 re f");
        page.media = [-2.0, -3.0, 98.0, 77.0];
        page.crop = Some(page.media);
        page.trim = Some(page.media);
        page.bleed = Some(page.media);
        page.art = Some([6.0, 5.0, 90.0, 69.0]);
        page.entries = "/Group <</S /Transparency /CS /DeviceRGB /I true /K false>>".into();
        pdf_with(&[page], &[], None)
    }

    #[test]
    fn page_boxes_and_blending_survive_native_save_and_export() {
        let read = import_with_report(&fixture(), &ImportOptions::default()).unwrap();
        let native = serde_json::to_vec(&read.document).unwrap();
        let doc: Document = serde_json::from_slice(&native).unwrap();
        let out = export(&doc, &PdfOptions::default()).unwrap();
        let pdf = crate::pages::open(&out, None).unwrap();
        let page = &pdf.pages()[0];
        assert_eq!(page.raw().get::<[f64; 4]>(b"ArtBox"), Some([6.0, 5.0, 90.0, 69.0]));
        assert_eq!(page.raw().get::<[f64; 4]>(b"MediaBox"), Some([-2.0, -3.0, 98.0, 77.0]));
        let group = page.raw().get::<Dict<'_>>(b"Group").unwrap();
        assert_eq!(group.get::<Name<'_>>(b"CS").unwrap().as_str(), "DeviceRGB");
        assert_eq!(group.get::<bool>(b"I"), Some(true));
        let back = import_with_report(&out, &ImportOptions::default()).unwrap();
        assert_eq!(back.document.artboards[0].rect, doc.artboards[0].rect);
        let bounds = |d: &Document| d.layers[0].visual_bounds().unwrap();
        assert_eq!(bounds(&back.document), bounds(&doc));
    }

    #[test]
    fn fractional_page_origin_and_path_coordinates_survive_export() {
        let mut page = PdfPage::new(
            100.0,
            80.0,
            "0 0 0 1 k 12.123456789 9.987654321 m 12.123456789 9.987654321 l 42.135791357 12.246813579 l 45.987654321 39.123456789 l h f",
        );
        page.media = [-0.565893, -0.566967, 924.660573, 167.067627];
        let bytes = pdf_with(&[page], &[], None);
        let opts = ImportOptions { crop: CropTo::Media, ..Default::default() };
        let doc = import_with_report(&bytes, &opts).unwrap().document;
        assert!((doc.artboards[0].rect.width() - 925.226466).abs() < 1e-10);
        let saved = serde_json::to_vec(&doc).unwrap();
        let doc: Document = serde_json::from_slice(&saved).unwrap();
        let out = export(&doc, &PdfOptions::default()).unwrap();
        let back = import_with_report(&out, &opts).unwrap().document;
        let a = doc.layers[0].visual_bounds().unwrap();
        let b = back.layers[0].visual_bounds().unwrap();
        for (a, b) in rect(a).into_iter().zip(rect(b)) {
            assert!((a - b).abs() < 1e-9, "{a} changed to {b}");
        }
    }

    #[test]
    fn large_imported_page_retains_physical_boxes_and_origin_with_user_units() {
        let mut page = PdfPage::new(20_000.0, 10_000.0, "0 0 0 1 k 12 9 30 20 re f");
        page.media = [-2.0, -3.0, 19_998.0, 9_997.0];
        page.crop = Some(page.media);
        page.art = Some([6.0, 5.0, 19_990.0, 9_990.0]);
        let bytes = pdf_with(&[page.clone()], &[], None);
        let opts = ImportOptions { crop: CropTo::Media, ..Default::default() };
        let doc = import_with_report(&bytes, &opts).unwrap().document;
        let out = export(&doc, &PdfOptions::default()).unwrap();
        let pdf = crate::pages::open(&out, None).unwrap();
        let output = &pdf.pages()[0];
        let unit = f64::from(output.user_unit());
        assert!(unit > 1.0);
        for (key, expected) in [(b"MediaBox".as_slice(), page.media), (b"ArtBox".as_slice(), page.art.unwrap())] {
            let actual = output.raw().get::<[f64; 4]>(key).unwrap();
            for (a, b) in actual.into_iter().zip(expected) {
                assert!((a * unit - b).abs() < 0.01, "{a} * {unit} != {b}");
            }
        }
        let back = import_with_report(&out, &opts).unwrap().document;
        for (a, b) in rect(doc.layers[0].visual_bounds().unwrap()).into_iter().zip(rect(back.layers[0].visual_bounds().unwrap())) {
            assert!((a - b).abs() < 0.01, "art moved: {a} != {b}");
        }
    }

    #[test]
    fn cmyk_artwork_keeps_rgb_transparency_blending_after_export() {
        let mut doc = import_with_report(&fixture(), &ImportOptions::default()).unwrap().document;
        assert_eq!(doc.color_mode, vectorcraft_doc::ColorMode::Cmyk);
        // Force both a page group and a nested group, as masked/trapped artwork does.
        doc.page_isolate = true;
        std::sync::Arc::make_mut(&mut doc.layers[0]).opacity = 0.75;
        let out = export(&doc, &PdfOptions::default()).unwrap();
        let parsed = crate::linearize::parse(&out).unwrap();
        let mut count = 0;
        for obj in parsed.objects.values() {
            if let Some(group) = obj.value.get(b"Group") {
                assert_eq!(group.name(b"CS"), Some(b"DeviceRGB".as_slice()));
                count += 1;
            }
        }
        assert!(count >= 2, "exercise nested and page blending spaces");
        // Opting out of imported settings restores the new CMYK document policy.
        let opts = PdfOptions { settings: crate::PdfSettings { discard_imported_print_settings: true, ..Default::default() }, ..Default::default() };
        let out = export(&doc, &opts).unwrap();
        let parsed = crate::linearize::parse(&out).unwrap();
        assert!(parsed.objects.values().filter_map(|o| o.value.get(b"Group")).any(|g| g.name(b"CS") == Some(b"DeviceCMYK")));
    }

    #[test]
    fn output_profile_bytes_survive_native_save_and_pdf_export() {
        let read = import_with_report(&fixture(), &ImportOptions::default()).unwrap();
        let mut doc = read.document;
        let mut state: State = serde_json::from_value(doc.unknown.get(KEY).unwrap().clone()).unwrap();
        let profile = vectorcraft_color::cms::icc_bytes(vectorcraft_color::cms::SRGB).unwrap().to_vec();
        state.intents.push(Intent {
            kind: "GTS_PDFX".into(),
            text: BTreeMap::from([("OutputConditionIdentifier".into(), "Original test condition".into())]),
            profile: profile.clone(),
            components: 3,
        });
        doc.unknown.insert(KEY.into(), serde_json::to_value(state).unwrap());
        let out = export(&doc, &PdfOptions::default()).unwrap();
        let read = import_with_report(&out, &ImportOptions::default()).unwrap();
        let retained: State = serde_json::from_value(read.document.unknown.get(KEY).unwrap().clone()).unwrap();
        assert_eq!(retained.intents[0].profile, profile);
        assert_eq!(retained.intents[0].text["OutputConditionIdentifier"], "Original test condition");
    }

    #[test]
    fn resizing_or_explicit_discard_does_not_restore_stale_boxes() {
        let mut doc = import_with_report(&fixture(), &ImportOptions::default()).unwrap().document;
        let opts = PdfOptions { settings: crate::PdfSettings { discard_imported_print_settings: true, ..Default::default() }, ..Default::default() };
        let out = export(&doc, &opts).unwrap();
        assert!(!crate::pages::open(&out, None).unwrap().pages()[0].raw().contains_key(b"ArtBox"));
        doc.artboards[0].rect.x1 += 10.0;
        let out = crate::export_with_report(&doc, &PdfOptions::default()).unwrap();
        assert!(out.warnings.iter().any(|w| w.contains("artboard, bleed or printer marks changed")));
        assert!(!crate::pages::open(&out.bytes, None).unwrap().pages()[0].raw().contains_key(b"ArtBox"));
    }
}

/// Store preparation page boxes while retaining the imported blending space and output intents.
/// Export coordinates start at the prepared media frame, so original PDF origin translations
/// must not be reapplied after preparation changes the page boxes.
pub fn set_preparation_boxes(doc: &mut Document, boxes: &BTreeMap<String, vectorcraft_geom::Rect>) -> Result<(), PdfError> {
    let board = doc.artboards.first().ok_or_else(|| error("missing preparation artboard"))?;
    let media = boxes.get("MediaBox").ok_or_else(|| error("missing preparation MediaBox"))?;
    let mut state: State = match doc.unknown.get(KEY) {
        Some(value) => serde_json::from_value(value.clone()).map_err(|_| error("invalid saved print settings"))?,
        None => State::default(),
    };
    let mut values = BTreeMap::new();
    for name in BOXES {
        if let Some(r) = boxes.get(name) {
            let v = [r.x0 - media.x0, media.y1 - r.y1, r.x1 - media.x0, media.y1 - r.y0];
            if !valid_box(&v) {
                return Err(error("invalid preparation page box"));
            }
            values.insert(name.into(), v);
        }
    }
    if let Some(page) = state.pages.iter_mut().find(|p| p.artboard == board.id) {
        page.boxes = values;
        page.origin = [0.0, 0.0];
        page.frame = rect(board.rect);
    } else {
        state.pages.push(Page { artboard: board.id, frame: rect(board.rect), origin: [0.0, 0.0], boxes: values, group: None });
    }
    doc.unknown.insert(KEY.into(), serde_json::to_value(state).map_err(|_| error("cannot store preparation page boxes"))?);
    Ok(())
}

/// Finish a single-page preparation PDF with explicit page boxes and declared spot inks.
/// Boxes use the document's top-left, y-down coordinates. Unpainted inks are resources only;
/// declaring an ink does not add marks or separation geometry to the artwork.
pub fn preparation_output(pdf: Vec<u8>, boxes: &BTreeMap<String, vectorcraft_geom::Rect>, doc: &Document) -> Result<Vec<u8>, PdfError> {
    let parsed = crate::linearize::parse(&pdf).map_err(error)?;
    if parsed.pages.len() != 1 {
        return Err(error("preparation output requires one page"));
    }
    let page = parsed.pages.first().and_then(|n| parsed.objects.get(n)).ok_or_else(|| error("missing preparation page"))?;
    let media = boxes.get("MediaBox").ok_or_else(|| error("missing preparation MediaBox"))?;
    let mut replacements = BTreeMap::new();
    for name in BOXES {
        if let Some(r) = boxes.get(name) {
            let v = [r.x0 - media.x0, media.y1 - r.y1, r.x1 - media.x0, media.y1 - r.y0];
            if !valid_box(&v) {
                return Err(error("invalid preparation page box"));
            }
            replacements.insert(name, format!("[{} {} {} {}]", v[0], v[1], v[2], v[3]));
        }
    }
    let resolve = |value: &Obj<'_>| match value {
        Obj::Ref(n, _) => parsed.objects.get(n).map(|o| &o.value),
        _ => None,
    };
    let resources = page.value.get(b"Resources").ok_or_else(|| error("missing preparation resources"))?;
    let resources = resolve(resources).unwrap_or(resources);
    let colors = resources.get(b"ColorSpace");
    let colors = colors.map(|c| resolve(c).unwrap_or(c));
    let mut entries = String::from("<<");
    if let Some(Obj::Dict { start, end, .. }) = colors {
        let body = pdf.get(start + 2..end - 2).ok_or_else(|| error("invalid resource dictionary span"))?;
        entries.push_str(std::str::from_utf8(body).map_err(|_| error("non-UTF8 color resources"))?);
    }
    for (i, spot) in doc.swatches.iter().filter(|s| s.spot).enumerate() {
        let color = spot.paint.color().ok_or_else(|| error("spot ink has no alternate color"))?;
        let [c, m, y, k] = color.to_cmyk();
        let name = spot.name.as_bytes().iter().map(|b| format!("#{b:02X}")).collect::<String>();
        entries.push_str(&format!(
            " /PALInk{i} [/Separation /{name} /DeviceCMYK <</FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [{c} {m} {y} {k}] /N 1>>]"
        ));
    }
    entries.push_str(" >>");
    let resources = dictionary(&pdf, resources, &BTreeMap::from([("ColorSpace", entries)]))?;
    replacements.insert("Resources", String::from_utf8(resources).map_err(|_| error("non-UTF8 resource dictionary"))?);
    let mut patch = Patch::new(&parsed.xref);
    patch.replace(page.value_span.0, page.value_span.1, dictionary(&pdf, &page.value, &replacements)?);
    patch.apply(&pdf, &parsed.xref)
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    use vectorcraft_color::{Color, Paint, Swatch};
    use vectorcraft_geom::Rect;
    use vectorcraft_testkit::pdf::{PdfPage, pdf};
    #[test]
    fn preparation_boxes_and_unused_ink_preserve_existing_page_paint() {
        let input = pdf(&[PdfPage::new(100.0, 80.0, "0 1 0 0 k 10 10 20 20 re f")], None);
        let mut doc = crate::import(&input).unwrap();
        doc.swatches.push(Swatch { name: "Ink / (one) é".into(), paint: Paint::solid(Color::rgb(1.0, 0.0, 1.0)), spot: true, global: true });
        let plain = crate::export(&doc, &PdfOptions::default()).unwrap();
        let boxes = BTreeMap::from([("MediaBox".into(), Rect::new(0.0, 0.0, 100.0, 80.0)), ("ArtBox".into(), Rect::new(10.0, 50.0, 30.0, 70.0))]);
        let output = preparation_output(plain, &boxes, &doc).unwrap();
        let info = crate::info(&output, None).unwrap();
        assert_eq!(info.pages[0].boxes.iter().find(|(b, _)| *b == CropTo::Art).unwrap().1, Rect::new(10.0, 10.0, 30.0, 30.0));
        let read = crate::import(&output).unwrap();
        assert_eq!(
            read.bounds_of(&read.layers.iter().map(|n| n.id).collect::<Vec<_>>(), true),
            doc.bounds_of(&doc.layers.iter().map(|n| n.id).collect::<Vec<_>>(), true)
        );
        assert!(String::from_utf8_lossy(&output).contains("#49#6E#6B#20#2F#20#28#6F#6E#65#29#20#C3#A9"));
    }
}

#[cfg(test)]
mod preparation_origin_tests {
    use super::*;
    use vectorcraft_geom::Rect;
    use vectorcraft_testkit::pdf::{PdfPage, pdf};
    #[test]
    fn preparation_resets_source_origin_without_shifting_art_or_discarding_blending() {
        let mut page = PdfPage::new(100.0, 80.0, "0 0 0 1 k 10 10 20 20 re f");
        page.media = [-5.0, -10.0, 95.0, 70.0];
        page.entries = "/Group <</S /Transparency /CS /DeviceRGB /I true /K false>>".into();
        let mut doc =
            crate::import_with_report(&pdf(&[page], None), &crate::ImportOptions { crop: CropTo::Media, ..Default::default() }).unwrap().document;
        let expected = doc.layers[0].visual_bounds();
        let boxes = BTreeMap::from([("MediaBox".into(), Rect::new(0.0, 0.0, 100.0, 80.0))]);
        set_preparation_boxes(&mut doc, &boxes).unwrap();
        let bytes = crate::export(&doc, &PdfOptions::default()).unwrap();
        let output = preparation_output(bytes, &boxes, &doc).unwrap();
        let reopened = crate::import(&output).unwrap();
        assert_eq!(expected, reopened.layers[0].visual_bounds());
        let pdf = crate::pages::open(&output, None).unwrap();
        let group = pdf.pages()[0].raw().get::<Dict<'_>>(b"Group").unwrap();
        assert_eq!(group.get::<Name<'_>>(b"CS").unwrap().as_str(), "DeviceRGB");
    }
}
