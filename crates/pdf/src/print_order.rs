//! A bounded content-stream order audit, before editor layer aggregation.
//! Records path order and image uses; this is not complete composited PDF qualification.
use crate::{
    PdfError,
    import_scan::{Ocgs, scan_print_order},
};
use serde::Serialize;
use std::collections::BTreeSet;
/// Source space before editable import converts a process-named colour to CMYK.
/// ISO 32000-1 §§8.6.5–8.6.7: OPM 1 zero-component suppression is specific to
/// direct DeviceCMYK, not Separation or DeviceN process colourants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrintColorSpace {
    #[default]
    DeviceGray,
    DeviceRgb,
    DeviceCmyk,
    Separation,
    DeviceN,
    Other,
}
#[derive(Clone, Debug, Serialize)]
pub struct PrintPaintSource {
    /// Raw operator kind, retained to detect interpreter fill/stroke substitutions.
    pub stroke: bool,
    /// Actual graphics-state flags, reconciled against the editable census.
    pub overprint: bool,
    pub overprint_mode: u8,
    pub color_space: PrintColorSpace,
    /// Decoded names, unchanged. Empty for device/unsupported spaces.
    pub colorants: Vec<String>,
    /// Original typed operands before editable colour conversion. None means the
    /// source space has no supported initial value; it never means black or zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<f64>>,
    /// Extra DeviceN/NChannel semantics are retained, not silently discarded.
    /// Their presence does not by itself invalidate names, flags or paint order.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub devicen_attributes: bool,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct PageOrder {
    pub source_name_bytes: usize,
    pub sample_cost: usize,
    pub images: Vec<crate::PrintImageUse>,
    pub layers: Vec<Option<usize>>,
    pub source_indices: Vec<usize>,
    pub sources: Vec<PrintPaintSource>,
    pub issues: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PrintOrder {
    /// Image uses in stream order, per page. Does not establish composited coverage.
    pub images: Vec<Vec<crate::PrintImageUse>>,
    /// Exact OCG names; events refer into this dictionary, retaining duplicates.
    pub layers: Vec<String>,
    pub pages: Vec<Vec<Option<usize>>>,
    /// Parallel to pages; indices refer to the bounded source-space dictionary.
    pub source_indices: Vec<Vec<usize>>,
    pub sources: Vec<PrintPaintSource>,
    pub issues: BTreeSet<String>,
}
pub(crate) const MAX_SOURCE_NAME_BYTES: usize = 4 * 1024 * 1024;

pub fn print_order(bytes: &[u8]) -> Result<PrintOrder, PdfError> {
    print_order_with_cost(bytes, 0)
}
fn print_order_with_cost(bytes: &[u8], initial_sample_cost: usize) -> Result<PrintOrder, PdfError> {
    let pdf = crate::pages::open_with_options(bytes, None, &craft_pdf::OpenOptions { max_input_bytes: 256 * 1024 * 1024, ..Default::default() })?;
    let mut ocgs = Ocgs::read(&pdf);
    let mut report = PrintOrder { images: vec![], layers: vec![], pages: vec![], source_indices: vec![], sources: vec![], issues: BTreeSet::new() };
    if pdf.pages().len() > 1000 {
        return Err(PdfError::Unsupported("Print-order page budget exceeded".into()));
    }
    let mut source_name_bytes = 0usize;
    let mut count = 0usize;
    let mut images = 0usize;
    let mut sample_cost = initial_sample_cost;
    for page in pdf.pages().iter() {
        if page.raw().get::<craft_pdf::syntax::object::Object<'_>>(b"Annots").is_some() {
            report.issues.insert("Annotation print appearances require direct census".into());
        }
        if page.raw().get::<craft_pdf::syntax::object::Dict<'_>>(b"Group").is_some() {
            report.issues.insert("Page transparency group colour space requires compositing reconciliation".into());
        }
        let result = scan_print_order(page, &mut ocgs, pdf.decode_limits().max_decoded_stream(), sample_cost)?;
        count += result.layers.len();
        sample_cost = result.sample_cost;
        if sample_cost > crate::image_samples::SAMPLE_BUDGET {
            return Err(PdfError::Unsupported("Image decoded sample budget exceeded across pages".into()));
        }
        images += result.images.len();
        if images > crate::print_image::MAX_IMAGES {
            return Err(PdfError::Unsupported("Print-image use budget exceeded".into()));
        }
        report.images.push(result.images);
        if count > 50_000 {
            return Err(PdfError::Unsupported("Print-order event budget exceeded".into()));
        }
        if report.sources.len() + result.sources.len() > 4096 {
            return Err(PdfError::Unsupported("Print-source space budget exceeded".into()));
        }
        source_name_bytes = source_name_bytes.saturating_add(result.source_name_bytes);
        if source_name_bytes > MAX_SOURCE_NAME_BYTES {
            return Err(PdfError::Unsupported("Print-source aggregate colourant-name budget exceeded".into()));
        }
        let offset = report.sources.len();
        report.source_indices.push(result.source_indices.into_iter().map(|i| i + offset).collect());
        report.sources.extend(result.sources);
        report.pages.push(result.layers);
        report.issues.extend(result.issues);
    }
    if ocgs.list.iter().any(|g| !g.on || !g.print) {
        report.issues.insert("Optional-content view/print states require independent reconciliation".into());
    }
    if ocgs.list.iter().any(|g| g.synthetic_name) {
        report.issues.insert("Synthesized PDF layer names cannot establish exact routing".into());
    }
    report.layers = ocgs.list.into_iter().map(|g| g.name).collect();
    if report.layers.iter().collect::<BTreeSet<_>>().len() != report.layers.len() {
        report.issues.insert("Duplicate PDF layer names cannot establish unique routing".into());
    }
    Ok(report)
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn document_sample_budget_preserves_earlier_uses_and_later_errors() {
        use vectorcraft_testkit::pdf::{PdfPage, first_extra, pdf_with};
        let mut page = PdfPage::new(100.0, 100.0, "/Im Do");
        page.resources = format!("/XObject << /Im {} 0 R >>", first_extra(5));
        let im = "<< /Length 1 /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray >>\nstream\nx\nendstream";
        let bytes = pdf_with(&vec![page; 5], &[im], None);
        let audit = print_order_with_cost(&bytes, crate::image_samples::SAMPLE_BUDGET - 8).unwrap();
        assert_eq!(audit.images.iter().filter(|p| p[0].samples.is_some()).count(), 2);
        assert_eq!(audit.images.iter().filter(|p| p[0].sample_error.as_ref().is_some_and(|s| s.contains("budget"))).count(), 3);
    }
    // Original calculator function: retain two supplied components and append zero Y/K.
    const TWO_INK_FUNCTION: &str = "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length 7 >>\nstream\n{ 0 0 }\nendstream";

    #[test]
    fn colour_values_follow_defaults_fill_stroke_and_saved_state() {
        use vectorcraft_testkit::pdf::{PdfPage, pdf_with};
        let mut page = PdfPage::new(
            72.0,
            72.0,
            "0 0 2 2 re f /DeviceCMYK cs 0 0 2 2 re f /S cs 0 0 2 2 re f q .25 scn .5 G 0 0 2 2 re B Q 0 0 2 2 re f /D cs 0 0 2 2 re f .2 .7 scn 0 0 2 2 re f",
        );
        page.resources = "/ColorSpace << /S [/Separation /Custom /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 1] /N 1 >>] /D [/DeviceN [/Cyan /Magenta] /DeviceCMYK 5 0 R] >>".into();
        let audit = print_order(&pdf_with(&[page], &[TWO_INK_FUNCTION], None)).unwrap();
        assert!(audit.issues.is_empty(), "{:?}", audit.issues);
        let values: Vec<_> = audit.source_indices[0].iter().map(|i| audit.sources[*i].components.clone().unwrap()).collect();
        assert_eq!(values, [vec![0.0], vec![0.0, 0.0, 0.0, 1.0], vec![1.0], vec![0.25], vec![0.5], vec![1.0], vec![1.0, 1.0], vec![0.2, 0.7]]);
    }

    #[test]
    fn devicen_attributes_keep_separate_sources_and_follow_saved_fill_stroke_state() {
        use vectorcraft_testkit::pdf::{PdfPage, pdf_with};
        let mut page = PdfPage::new(
            72.0,
            72.0,
            "/A cs .25 .75 scn 0 0 2 2 re f q /B cs .25 .75 scn 0 0 2 2 re f /A CS .25 .75 SCN 0 0 2 2 re S 0 G 0 0 2 2 re S Q 0 0 2 2 re f /B cs .25 .75 scn 0 0 2 2 re f 0 g 0 0 2 2 re f",
        );
        page.resources = "/ColorSpace << /A [/DeviceN [/Cyan /Magenta] /DeviceCMYK 5 0 R << /Subtype /NChannel >>] /B [/DeviceN [/Cyan /Magenta] /DeviceCMYK 5 0 R] >>".into();
        let audit = print_order(&pdf_with(&[page], &[TWO_INK_FUNCTION], None)).unwrap();
        assert!(audit.issues.is_empty(), "Attributes must not invalidate structural names/order: {:?}", audit.issues);
        let sources: Vec<_> = audit.source_indices[0].iter().map(|i| &audit.sources[*i]).collect();
        assert_eq!(sources.iter().map(|s| s.devicen_attributes).collect::<Vec<_>>(), [true, false, true, false, true, false, false]);
        assert_ne!(audit.source_indices[0][0], audit.source_indices[0][1]);
        assert_eq!(sources[0].components, sources[1].components);
        assert_eq!(sources[0].colorants, sources[1].colorants);
        let mut nested_page = PdfPage::new(72.0, 72.0, "/A cs .25 .75 scn /Outer Do 0 0 2 2 re f");
        nested_page.resources = "/ColorSpace << /A [/DeviceN [/Cyan /Magenta] /DeviceCMYK 5 0 R 6 0 R] >> /XObject << /Outer 7 0 R >>".into();
        let outer_content = "0 0 2 2 re f q /B cs .25 .75 scn /Inner Do Q 0 0 2 2 re f";
        let outer = format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 72 72] /Resources << /ColorSpace << /B [/DeviceN [/Cyan /Magenta] /DeviceCMYK 5 0 R] >> /XObject << /Inner 8 0 R >> >> /Length {} >>\nstream\n{outer_content}\nendstream",
            outer_content.len()
        );
        let inner_content = "0 0 2 2 re f";
        let inner = format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 72 72] /Resources << >> /Length {} >>\nstream\n{inner_content}\nendstream",
            inner_content.len()
        );
        let nested = print_order(&pdf_with(&[nested_page], &[TWO_INK_FUNCTION, "<< /Subtype /NChannel >>", &outer, &inner], None)).unwrap();
        assert!(nested.issues.is_empty(), "{:?}", nested.issues);
        assert_eq!(nested.source_indices[0].iter().map(|i| nested.sources[*i].devicen_attributes).collect::<Vec<_>>(), [true, false, true, true]);
        let mut missing_page = PdfPage::new(72.0, 72.0, "/D cs .25 .75 scn 0 0 2 2 re f");
        missing_page.resources = "/ColorSpace << /D [/DeviceN [/Cyan /Magenta] /DeviceCMYK 5 0 R 99 0 R] >>".into();
        let missing = print_order(&pdf_with(&[missing_page], &[TWO_INK_FUNCTION], None)).unwrap();
        assert!(missing.sources[missing.source_indices[0][0]].devicen_attributes);
    }

    #[test]
    fn named_colour_identity_cannot_discard_or_replace_names() {
        use vectorcraft_testkit::pdf::{PdfPage, pdf_with};
        for (names, issue) in [
            ("[/Cyan 42 /Magenta]", "non-name"),
            ("[/Cyan /Cyan]", "duplicate decoded"),
            ("[/Cyan /C#79an]", "duplicate decoded"),
            ("[/Cyan /Bad#FF]", "losslessly"),
            ("[/Cyan /]", "losslessly"),
            ("[]", "1–32"),
        ] {
            let mut page = PdfPage::new(72.0, 72.0, "/D cs 1 1 scn 0 0 2 2 re f");
            page.resources = format!("/ColorSpace << /D [/DeviceN {names} /DeviceCMYK 5 0 R] >>");
            let audit = print_order(&pdf_with(&[page], &[TWO_INK_FUNCTION], None)).unwrap();
            assert!(audit.issues.iter().any(|s| s.contains(issue)), "{names}: {:?}", audit.issues);
        }
    }
    #[test]
    fn raw_colour_interning_and_retained_name_budgets_are_aggregate() {
        use vectorcraft_testkit::pdf::{PdfPage, pdf_with};
        let content = |count: usize| (0..count).map(|i| format!("{} g 0 0 1 1 re f\n", i as f64 / 8192.0)).collect::<String>();
        assert!(print_order(&pdf_with(&[PdfPage::new(72.0, 72.0, &content(4096))], &[], None)).is_ok());
        assert!(print_order(&pdf_with(&[PdfPage::new(72.0, 72.0, &content(4097))], &[], None)).is_err());
        let page = |count: usize| {
            let stream: String = (0..count).map(|i| format!("{} scn 0 0 1 1 re f\n", i as f64 / 128.0)).collect();
            let mut page = PdfPage::new(72.0, 72.0, &format!("/S cs {stream}"));
            page.resources = format!(
                "/ColorSpace << /S [/Separation /{} /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 1] /N 1 >>] >>",
                "a".repeat(64 * 1024)
            );
            page
        };
        assert!(print_order(&pdf_with(&[page(64)], &[], None)).is_ok());
        assert!(print_order(&pdf_with(&[page(65)], &[], None)).is_err());
        assert!(print_order(&pdf_with(&[page(32), page(32)], &[], None)).is_ok());
        let error = print_order(&pdf_with(&[page(32), page(32), page(1)], &[], None)).unwrap_err().to_string();
        assert!(error.contains("aggregate colourant-name budget"), "{error}");
    }

    #[test]
    fn vector_transfer_and_device_state_is_unresolved_only_when_painted() {
        use vectorcraft_testkit::pdf::{PdfPage, pdf_with};
        for key in ["TR", "TR2", "BG", "BG2", "UCR", "UCR2", "HT", "HTP", "RI"] {
            let make = |stream: &str| {
                let mut page = PdfPage::new(72.0, 72.0, stream);
                page.resources = format!("/ExtGState << /GS << /{key} /Unqualified >> >>");
                pdf_with(&[page], &[], None)
            };
            let audit = print_order(&make("/GS gs 0 0 24 24 re f")).unwrap();
            assert!(audit.issues.iter().any(|s| s.contains("Paint transfer")), "{key}");
            let restored = print_order(&make("q /GS gs Q 0 0 24 24 re f")).unwrap();
            assert!(restored.issues.is_empty(), "{key}: {:?}", restored.issues);
        }
        let mut page = PdfPage::new(72.0, 72.0, "/GS gs 0 0 24 24 re f");
        page.resources = "/ExtGState << /GS << /TR /Identity >> >>".into();
        assert!(print_order(&pdf_with(&[page], &[], None)).unwrap().issues.is_empty());
        for key in ["TR2", "BG2", "UCR2", "HT", "TR", "BG", "UCR", "RI", "HTP"] {
            let mut page = PdfPage::new(72.0, 72.0, "/GS gs 0 0 24 24 re f");
            page.resources = format!("/ExtGState << /GS << /{key} /Default >> >>");
            let audit = print_order(&pdf_with(&[page], &[], None)).unwrap();
            assert_eq!(audit.issues.is_empty(), matches!(key, "TR2" | "BG2" | "UCR2" | "HT"), "{key}");
        }
    }
}
