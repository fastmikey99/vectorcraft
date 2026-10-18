//! Required stacking is checked against actual PDF content order, before layer aggregation.
use crate::{Evidence, Report, Request, Status};
use serde_json::json;

/// Match path-paint events to decoded per-layer paths without trusting the editor's
/// cross-layer aggregation. Any omission, ambiguity or unsupported paint blocks the proof.
fn indices(order: &vectorcraft_pdf::PrintOrder, census: &crate::pdf::Census) -> Result<Vec<usize>, String> {
    use std::collections::{BTreeMap, VecDeque};
    if !order.issues.is_empty() || !census.unresolved.is_empty() || census.budget_exhausted || order.pages.len() != census.pages {
        return Err("Printable census is incomplete; content order cannot be reconciled".into());
    }
    if order.source_indices.len() != order.pages.len()
        || order
            .source_indices
            .iter()
            .zip(&order.pages)
            .any(|(sources, events)| sources.len() != events.len() || sources.iter().any(|i| *i >= order.sources.len()))
    {
        return Err("Raw PDF source-space census differs from paint events".into());
    }
    let mut groups: BTreeMap<(usize, Option<usize>), VecDeque<usize>> = BTreeMap::new();
    for (i, p) in census.paints.iter().enumerate() {
        let page = p.page.ok_or("Paint has no resolved production page")?;
        if p.geometry_mm.is_none() {
            return Err("Paint has no resolved coverage".into());
        }
        let layer = order.layers.iter().position(|name| name == &p.layer);
        groups.entry((page, layer)).or_default().push_back(i);
    }
    let mut indices = vec![];
    for (page, events) in order.pages.iter().enumerate() {
        for layer in events {
            let i = groups.get_mut(&(page, *layer)).and_then(VecDeque::pop_front).ok_or("Raw PDF path-paint count differs from decoded coverage")?;
            indices.push(i);
        }
    }
    if groups.values().any(|g| !g.is_empty()) {
        return Err("Decoded coverage contains unmatched path paints".into());
    }
    Ok(indices)
}
/// Restore real PDF stream order before compositing native per-ink predictions.
pub fn reconcile(bytes: &[u8], census: &mut crate::pdf::Census) {
    if let Err(reason) = try_reconcile(bytes, census) {
        census.unresolved.push(reason);
    }
}
pub(crate) fn try_reconcile(bytes: &[u8], census: &mut crate::pdf::Census) -> Result<(), String> {
    census.paint_order_verified = false;
    census.raw_audit = None;
    census.raw_audit_error = None;
    census.paint_sources.clear();
    for p in &mut census.paints {
        p.source_space = None;
    }
    let mut order = vectorcraft_pdf::print_order(bytes).map_err(|e| {
        let reason = e.to_string();
        census.raw_audit_error = Some(reason.clone());
        reason
    })?;
    let images: usize = order.images.iter().map(Vec::len).sum();
    if images != census.image_objects {
        order.issues.insert(format!(
            "Raw image uses ({images}) differ from decoded image objects ({}); expansion, omission or mask conversion needs reconciliation",
            census.image_objects
        ));
    }
    census.raw_audit = Some(crate::pdf::RawPaintAudit {
        path_paint_count: order.pages.iter().map(Vec::len).sum(),
        image_use_count: order.images.iter().map(Vec::len).sum(),
        source_space_count: order.sources.len(),
        image_sample: order
            .images
            .iter()
            .enumerate()
            .flat_map(|(page, images)| images.iter().map(move |image| (page, image.clone())))
            .take(32)
            .collect(),
        issue_count: order.issues.len(),
        issues_sample: order.issues.iter().take(64).cloned().collect(),
    });
    let indices = indices(&order, census)?;
    census.paints = indices.iter().filter_map(|i| census.paints.get(*i).cloned()).collect();
    for ((i, p), &source) in census.paints.iter_mut().enumerate().zip(order.source_indices.iter().flatten()) {
        p.order = i;
        p.source_space = Some(source);
    }
    census.paint_sources = order.sources;
    census.paint_order_verified = true;
    Ok(())
}

pub fn check(request: &Request, bytes: &[u8], census: &crate::pdf::Census, report: &mut Report) -> Result<(), String> {
    let mut order = match vectorcraft_pdf::print_order(bytes) {
        Ok(order) => order,
        Err(reason) => {
            if report.findings.iter().any(|f| f.obligation.key == "global/file_structure" && f.status == Status::Fail) {
                return Ok(());
            }
            report.record(
                "global/file_structure",
                Status::Incomplete,
                format!("Print-order census failed: {reason}"),
                serde_json::Value::Null,
                vec![],
            )?;
            return Ok(());
        }
    };
    match indices(&order, census) {
        Ok(indices) => {
            // Empty or fully clipped paths have no physical paint-order relationship.
            let mut cursor = 0;
            for page in &mut order.pages {
                page.retain(|_| {
                    let nonempty =
                        indices.get(cursor).and_then(|i| census.paints.get(*i)).and_then(|p| p.geometry_mm.as_ref()).is_some_and(|p| !p.is_empty());
                    cursor += 1;
                    nonempty
                });
            }
        }
        Err(reason) => {
            order.issues.insert(reason);
        }
    }
    let evidence = vec![Evidence {
        artifact_sha256: request.identity.pdf_sha256.clone(),
        producer: "checks/public-content-order-1".into(),
        detail: "Bounded path-paint stream order, including nested form content; independent of editable layer ordering".into(),
    }];
    let inactive =
        |key: &str| report.findings.iter().any(|f| f.obligation.key == format!("file/{key}/contribution") && f.status == Status::NotApplicable);
    let active: Vec<_> = request.recipe.file_links.iter().filter(|l| request.garment.matches(&l.garment) && !inactive(&l.key)).collect();
    let ranges: Vec<std::collections::BTreeMap<usize, (usize, usize)>> = order
        .pages
        .iter()
        .map(|events| {
            let mut ranges = std::collections::BTreeMap::new();
            for (i, layer) in events.iter().enumerate() {
                if let Some(layer) = layer {
                    ranges.entry(*layer).and_modify(|r: &mut (usize, usize)| r.1 = i).or_insert((i, i));
                }
            }
            ranges
        })
        .collect();
    let mut any_failure = false;
    for link in &active {
        let layer = order.layers.iter().position(|s| s == &link.pdf_layer);
        let mut findings = vec![];
        let mut checked = 0u64;
        let mut wrong = 0u64;
        let mut incomplete = !order.issues.is_empty() || layer.is_none();
        let mut failed = false;
        let mut found = false;
        for (page, ranges) in ranges.iter().enumerate() {
            let Some(&(first, last)) = layer.and_then(|id| ranges.get(&id)) else {
                continue;
            };
            found = true;
            for other in active.iter().filter(|o| o.pdf_layer != link.pdf_layer && o.layer_order != link.layer_order) {
                checked += 1;
                let other_layer = order.layers.iter().position(|s| s == &other.pdf_layer);
                let Some(&(other_first, other_last)) = other_layer.and_then(|id| ranges.get(&id)) else {
                    incomplete = true;
                    if findings.len() < 128 {
                        findings.push(json!({"page":page,"other":other.key,"reason":"Other applicable output is absent on this page; source-page applicability requires resolution"}));
                    }
                    continue;
                };
                let correct = if link.layer_order < other.layer_order { last < other_first } else { other_last < first };
                failed |= !correct;
                wrong += u64::from(!correct);
                if findings.len() >= 128 && !correct {
                    findings.pop();
                }
                if findings.len() < 128 {
                    findings.push(
                    json!({"page":page,"other":other.key,"correct":correct,"paint_range":[first,last],"other_paint_range":[other_first,other_last]}),
                );
                }
            }
        }
        let status = if !order.issues.is_empty() {
            Status::Incomplete
        } else if failed {
            Status::Fail
        } else if incomplete || !found {
            Status::Incomplete
        } else {
            Status::Pass
        };
        any_failure |= status == Status::Fail;
        report.record(
            &format!("file/{}/order", link.key),
            status,
            "Actual path paint sequence compared with the recipe's declared layer order; missing or unsupported content cannot establish order",
            json!({"comparisons_sample":findings,"comparisons_checked":checked,"wrong_order_count":wrong,"issues":order.issues,"layer":link.pdf_layer}),
            evidence.clone(),
        )?;
    }
    if any_failure {
        report.record(
            "global/file_structure",
            Status::Fail,
            "PDF paints required outputs out of order",
            json!({"actual_order_failed":true}),
            evidence,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Original hand-authored ISO PDF fixture. OCG /Order is unchanged when content interleaves.
    fn pdf(interleaved: bool, extra: &str) -> Vec<u8> {
        let stream = format!(
            "/OC /Lower BDC /Span BMC 0 0 10 10 re f EMC EMC /OC /Upper BDC 10 10 10 10 re f EMC {} {extra}",
            if interleaved { "/OC /Lower BDC 20 20 10 10 re f EMC" } else { "" }
        );
        sequence_pdf(&stream)
    }
    fn sequence_pdf(stream: &str) -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R] /D << /Order [6 0 R 5 0 R] /ON [5 0 R 6 0 R] >> >> >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /Lower 5 0 R /Upper 6 0 R >> >> /Contents 4 0 R >>"
                .into(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /OCG /Name (Layer) >>".into(),
            "<< /Type /OCG /Name (Upper) >>".into(),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![0];
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
        for offset in offsets.iter().skip(1) {
            bytes.extend(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len()).as_bytes());
        bytes
    }
    #[test]
    fn image_audit_survives_unresolved_reconciliation_and_exposes_omissions() {
        let bytes = pdf(false, "BI /W 1 /H 1 /BPC 8 /CS /G ID x EI");
        let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        assert_eq!(census.image_objects, 1);
        assert!(try_reconcile(&bytes, &mut census).is_err());
        assert!(!census.paint_order_verified);
        let raw = census.raw_audit.as_ref().unwrap();
        assert_eq!(raw.image_use_count, 1);
        assert_eq!(raw.path_paint_count, 2);
        assert_eq!(raw.image_sample.len(), 1);
        let mut request = crate::tests::request();
        request.identity.pdf_sha256 = crate::sha256(&bytes);
        request.identity.recipe_sha256 = crate::digest(&request.recipe).unwrap();
        let report = crate::pdf::check(&request, &bytes).unwrap();
        let integrity = report.findings.iter().find(|f| f.obligation.key == "global/integrity").unwrap();
        assert_eq!(integrity.status, Status::Incomplete);
        assert_eq!(integrity.observed["raw_paint_audit"]["image_use_count"], 1);
        assert_eq!(integrity.observed["decoded_image_objects"], 1);
        // A dropped imported image cannot erase its raw occurrence or establish order.
        census.image_objects = 0;
        assert!(try_reconcile(&bytes, &mut census).is_err());
        let raw = census.raw_audit.as_ref().unwrap();
        assert!(raw.issues_sample.iter().any(|s| s.contains("differ from decoded image objects")));
        assert_eq!(raw.image_use_count, 1);
    }
    #[test]
    fn image_samples_are_bounded_while_the_full_count_is_retained() {
        let bytes = pdf(false, &"BI /W 1 /H 1 /BPC 8 /CS /G ID x EI ".repeat(35));
        let mut census = crate::pdf::census(&vectorcraft_doc::Document::new(100.0, 100.0));
        assert!(try_reconcile(&bytes, &mut census).is_err());
        let raw = census.raw_audit.unwrap();
        assert_eq!(raw.image_use_count, 35);
        assert_eq!(raw.image_sample.len(), 32);
    }
    #[test]
    fn raw_image_budget_failure_is_retained_as_an_explicit_audit_error() {
        let bytes = pdf(false, &"BI /W 1 /H 1 /BPC 8 /CS /G ID x EI ".repeat(4097));
        let mut census = crate::pdf::census(&vectorcraft_doc::Document::new(100.0, 100.0));
        assert!(try_reconcile(&bytes, &mut census).is_err());
        assert!(census.raw_audit.is_none());
        assert!(census.raw_audit_error.is_some());
        assert!(!census.paint_order_verified);
    }
    #[test]
    fn interleaved_content_fails_even_when_layer_metadata_looks_right() {
        for interleaved in [false, true] {
            let bytes = pdf(interleaved, "");
            let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
            assert!(raw.issues.is_empty(), "{:?}", raw.issues);
            let names: Vec<_> = raw.pages[0].iter().map(|i| raw.layers[i.unwrap()].as_str()).collect();
            assert_eq!(names, if interleaved { vec!["Layer", "Upper", "Layer"] } else { vec!["Layer", "Upper"] });
            let mut request = crate::tests::request();
            request.identity.pdf_sha256 = crate::sha256(&bytes);
            request.recipe.file_links[0].layer_order = 0;
            let mut upper = request.recipe.file_links[0].clone();
            upper.key = "upper".into();
            upper.pdf_layer = "Upper".into();
            upper.layer_order = 1;
            request.recipe.file_links.push(upper);
            let mut report = crate::begin(&request).unwrap();
            let census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
            check(&request, &bytes, &census, &mut report).unwrap();
            for key in ["file/output/order", "file/upper/order"] {
                assert_eq!(
                    report.findings.iter().find(|f| f.obligation.key == key).unwrap().status,
                    if interleaved { Status::Fail } else { Status::Pass }
                );
            }
        }
    }
    #[test]
    fn unbalanced_content_and_images_cannot_establish_paint_order() {
        for extra in ["/Span BMC", "q 1 0 0 1 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G ID x EI Q"] {
            let raw = vectorcraft_pdf::print_order(&pdf(false, extra)).unwrap();
            assert!(!raw.issues.is_empty(), "{extra}");
        }
    }
    #[test]
    fn reconciles_layer_aggregation_and_rejects_omitted_paints() {
        let bytes = sequence_pdf(
            "/OC /Lower BDC 1 0 0 0 k 10 10 20 20 re f EMC /OC /Upper BDC 0 1 0 0 k 10 10 20 20 re f EMC /OC /Lower BDC 1 0 0 0 k 10 10 20 20 re f EMC",
        );
        let mut c = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        let before = c.clone();
        reconcile(&bytes, &mut c);
        assert!(c.unresolved.is_empty(), "{:?}", c.unresolved);
        assert_eq!(c.paints.iter().map(|p| p.layer.as_str()).collect::<Vec<_>>(), vec!["Layer", "Upper", "Layer"]);
        assert_eq!(c.paints.iter().map(|p| p.paint["color"]["c"].as_f64().unwrap()).collect::<Vec<_>>(), vec![1.0, 0.0, 1.0]);
        assert!(c.paint_order_verified);
        let draft = crate::prepare::capture(&bytes, "synthetic-original".into(), "job-1".into()).unwrap();
        let mut original: Vec<_> = draft.geometry.regions.values().collect();
        original.sort_by_key(|p| p.source_order);
        assert_eq!(original.iter().map(|p| p.source_order).collect::<Vec<_>>(), vec![Some(0), Some(1), Some(2)]);
        assert_eq!(original.iter().map(|p| p.paint["color"]["c"].as_f64().unwrap()).collect::<Vec<_>>(), vec![1.0, 0.0, 1.0]);
        c.paints.pop();
        reconcile(&bytes, &mut c);
        assert!(!c.unresolved.is_empty());
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(binary) = std::env::var("CHECK_GS_BINARY") {
            let output = std::env::temp_dir()
                .join(format!("vc-order-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            let config = crate::independent::Configuration {
                binary: binary.clone().into(),
                binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                version: "10.07.1".into(),
                dpi: 144,
                timeout_ms: 10_000,
                max_output_bytes: 16 * 1024 * 1024,
                qualification_sha256: None,
            };
            let render = crate::independent::render(&config, &bytes, &output).unwrap();
            assert_eq!(crate::raster::compare(&before, &render, Some(0.3)).unwrap().status, Status::Incomplete);
            let mut after = before;
            reconcile(&bytes, &mut after);
            assert_eq!(crate::raster::compare(&after, &render, Some(0.3)).unwrap().status, Status::Pass);
            std::fs::remove_dir_all(output).unwrap();
        }
    }
    #[test]
    fn unresolved_optional_content_does_not_claim_order() {
        let bytes = sequence_pdf("/OC /Missing BDC 0 0 10 10 re f EMC");
        assert!(!vectorcraft_pdf::print_order(&bytes).unwrap().issues.is_empty());
    }
}
