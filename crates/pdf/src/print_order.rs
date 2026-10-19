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
}
#[derive(Clone, Debug, Default)]
pub(crate) struct PageOrder {
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
