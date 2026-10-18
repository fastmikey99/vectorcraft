//! A bounded content-stream order audit, before editor layer aggregation.
//! This is a path-paint ordering proof, not complete PDF interpretation/qualification.
use crate::{
    PdfError,
    import_scan::{Ocgs, scan_print_order},
};
use serde::Serialize;
use std::collections::BTreeSet;
#[derive(Clone, Debug, Default)]
pub(crate) struct PageOrder {
    pub layers: Vec<Option<usize>>,
    pub issues: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PrintOrder {
    /// Exact OCG names; events refer into this dictionary, retaining duplicates.
    pub layers: Vec<String>,
    pub pages: Vec<Vec<Option<usize>>>,
    pub issues: BTreeSet<String>,
}
pub fn print_order(bytes: &[u8]) -> Result<PrintOrder, PdfError> {
    let pdf = crate::pages::open_with_options(bytes, None, &craft_pdf::OpenOptions { max_input_bytes: 256 * 1024 * 1024, ..Default::default() })?;
    let mut ocgs = Ocgs::read(&pdf);
    let mut report = PrintOrder { layers: vec![], pages: vec![], issues: BTreeSet::new() };
    if pdf.pages().len() > 1000 {
        return Err(PdfError::Unsupported("Print-order page budget exceeded".into()));
    }
    let mut count = 0usize;
    for page in pdf.pages().iter() {
        if page.raw().get::<craft_pdf::syntax::object::Object<'_>>(b"Annots").is_some() {
            report.issues.insert("Annotation print appearances require direct census".into());
        }
        let result = scan_print_order(page, &mut ocgs, pdf.decode_limits().max_decoded_stream())?;
        count += result.layers.len();
        if count > 50_000 {
            return Err(PdfError::Unsupported("Print-order event budget exceeded".into()));
        }
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
