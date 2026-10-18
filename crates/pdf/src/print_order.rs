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
    pub color_space: PrintColorSpace,
    /// Decoded names, unchanged. Empty for device/unsupported spaces.
    pub colorants: Vec<String>,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct PageOrder {
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
    let pdf = crate::pages::open_with_options(bytes, None, &craft_pdf::OpenOptions { max_input_bytes: 256 * 1024 * 1024, ..Default::default() })?;
    let mut ocgs = Ocgs::read(&pdf);
    let mut report = PrintOrder { images: vec![], layers: vec![], pages: vec![], source_indices: vec![], sources: vec![], issues: BTreeSet::new() };
    if pdf.pages().len() > 1000 {
        return Err(PdfError::Unsupported("Print-order page budget exceeded".into()));
    }
    let mut count = 0usize;
    let mut images = 0usize;
    for page in pdf.pages().iter() {
        if page.raw().get::<craft_pdf::syntax::object::Object<'_>>(b"Annots").is_some() {
            report.issues.insert("Annotation print appearances require direct census".into());
        }
        if page.raw().get::<craft_pdf::syntax::object::Dict<'_>>(b"Group").is_some() {
            report.issues.insert("Page transparency group colour space requires compositing reconciliation".into());
        }
        let result = scan_print_order(page, &mut ocgs, pdf.decode_limits().max_decoded_stream())?;
        count += result.layers.len();
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
    report.layers = ocgs.list.into_iter().map(|g| g.name).collect();
    if report.layers.iter().collect::<BTreeSet<_>>().len() != report.layers.len() {
        report.issues.insert("Duplicate PDF layer names cannot establish unique routing".into());
    }
    Ok(report)
}
