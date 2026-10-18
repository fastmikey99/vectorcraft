//! Inspect freshly decoded saved PDF bytes, never editor labels or hidden preview state.
//! Flag/structure findings are provisional; effective ink coverage is a separate obligation.
use crate::{Evidence, Report, Request, Status, begin, preparation, sha256};
use serde::Serialize;
use serde_json::{Value, json};
use vectorcraft_color::{BlendMode, Paint};
use vectorcraft_doc::{Document, Node, NodeKind, SymbolBoxes};
use vectorcraft_geom::{Affine, PathData, Shape};

#[derive(Clone, Debug, Serialize)]
pub struct PaintedObject {
    pub id: u64,
    pub layer: String,
    pub ink: Option<String>,
    pub process: bool,
    pub overprint: bool,
    pub overprint_zero: bool,
    pub tint: Option<f32>,
    pub bounds_mm: Option<[f64; 4]>,
    pub kind: String,
    pub order: usize,
    pub source_space: Option<usize>,
    pub page: Option<usize>,
    pub geometry_mm: Option<PathData>,
    pub geometry_error_mm: f64,
    pub paint: Value,
}
#[derive(Clone, Debug, Serialize)]
pub struct RawPaintAudit {
    pub path_paint_count: usize,
    pub image_use_count: usize,
    pub source_space_count: usize,
    pub image_sample: Vec<(usize, vectorcraft_pdf::PrintImageUse)>,
    pub issue_count: usize,
    pub issues_sample: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Census {
    pub paint_order_verified: bool,
    pub raw_audit: Option<RawPaintAudit>,
    pub raw_audit_error: Option<String>,
    pub image_objects: usize,
    pub paint_sources: Vec<vectorcraft_pdf::PrintPaintSource>,
    pub unresolved_layers: std::collections::BTreeSet<String>,
    pub paints: Vec<PaintedObject>,
    pub unresolved: Vec<String>,
    pub pages: usize,
    pub page_sizes_mm: Vec<[f64; 2]>,
    pub budget_exhausted: bool,
    pub object_count: usize,
    pub unaccounted_objects: Vec<String>,
}
#[derive(Clone, Copy)]
struct Traversal<'a> {
    visible: bool,
    depth: usize,
    clip: Option<&'a PathData>,
}
fn filled(path: &PathData, rule: vectorcraft_geom::FillRule) -> Result<PathData, String> {
    vectorcraft_pathops::try_boolean_for_measurement(
        path,
        rule,
        &PathData::default(),
        vectorcraft_geom::FillRule::NonZero,
        vectorcraft_pathops::BoolOp::Union,
    )
    .map_err(|e| e.to_string())
}
fn intersect(path: &PathData, clip: &PathData) -> Result<PathData, String> {
    vectorcraft_pathops::try_boolean_for_measurement(
        path,
        vectorcraft_geom::FillRule::NonZero,
        clip,
        vectorcraft_geom::FillRule::NonZero,
        vectorcraft_pathops::BoolOp::Intersect,
    )
    .map_err(|e| e.to_string())
}
fn stroke_coverage(path: &vectorcraft_geom::BezPath, stroke: &vectorcraft_doc::StrokeLayer) -> Result<PathData, String> {
    use vectorcraft_doc::StrokeAlign;
    if !stroke.width.is_finite() || stroke.width <= 0.0 {
        return Err("Device-dependent hairline requires raster measurement at the production device resolution".into());
    }
    if stroke.align != StrokeAlign::Center
        || stroke.start_arrow.is_some()
        || stroke.end_arrow.is_some()
        || stroke.profile.is_some()
        || stroke.brush.is_some()
        || stroke.path_gradient().is_some()
        || !stroke.miter_limit.is_finite()
        || stroke.miter_limit < 1.0
    {
        return Err("Non-PDF stroke appearance requires separately verified expansion".into());
    }
    if path.elements().len() > 100_000 {
        return Err("Stroke path resource budget exceeded".into());
    }
    let finite = |p: vectorcraft_geom::Point| p.x.is_finite() && p.y.is_finite() && p.x.abs() <= 1.0e9 && p.y.abs() <= 1.0e9;
    if stroke.width > 1.0e9
        || stroke.miter_limit > 1.0e6
        || path.elements().iter().any(|el| match el {
            vectorcraft_geom::PathEl::MoveTo(p) | vectorcraft_geom::PathEl::LineTo(p) => !finite(*p),
            vectorcraft_geom::PathEl::QuadTo(a, b) => !finite(*a) || !finite(*b),
            vectorcraft_geom::PathEl::CurveTo(a, b, c) => !finite(*a) || !finite(*b) || !finite(*c),
            vectorcraft_geom::PathEl::ClosePath => false,
        })
    {
        return Err("Stroke geometry exceeds finite coordinate bounds".into());
    }
    if let Some(dash) = &stroke.dash {
        let cycle: f64 = dash.pattern.iter().sum();
        if dash.pattern.len() > 1024
            || dash.align_corners
            || !dash.offset.is_finite()
            || dash.pattern.iter().any(|n| !n.is_finite() || *n < 0.0)
            || (!dash.pattern.is_empty() && (!cycle.is_finite() || cycle <= 0.0))
        {
            return Err("Invalid or unsupported stroke dash pattern".into());
        }
        // Control-polygon length bounds arc length without flattening tiny features.
        let length: f64 = path
            .segments()
            .map(|s| match s {
                vectorcraft_geom::PathSeg::Line(l) => l.p0.distance(l.p1),
                vectorcraft_geom::PathSeg::Quad(q) => q.p0.distance(q.p1) + q.p1.distance(q.p2),
                vectorcraft_geom::PathSeg::Cubic(c) => c.p0.distance(c.p1) + c.p1.distance(c.p2) + c.p2.distance(c.p3),
            })
            .sum();
        if !dash.pattern.is_empty() && (!length.is_finite() || length / cycle * dash.pattern.len() as f64 > 50_000.0) {
            return Err("Stroke dash expansion resource budget exceeded".into());
        }
    }
    let outline = vectorcraft_effects::stroke::line_outline(path, stroke, stroke.width, 0.0001);
    if outline.elements().len() > 100_000 {
        return Err("Stroke expansion resource budget exceeded".into());
    }
    filled(&PathData::from_bezpath(&outline), vectorcraft_geom::FillRule::NonZero)
}
fn walk(doc: &Document, symbols: &SymbolBoxes, node: &Node, layer: &str, state: Traversal<'_>, census: &mut Census) {
    if census.object_count >= 50_000 {
        census.budget_exhausted = true;
        return;
    }
    census.object_count += 1;
    if state.depth > 256 {
        census.unresolved_layers.insert(layer.into());
        census.unresolved.push("Object nesting exceeds census budget".into());
        return;
    }
    let mut layer = layer;
    let visible = state.visible && node.visible;
    if let NodeKind::Layer { printable, template, .. } = &node.kind {
        layer = node.name.as_deref().unwrap_or("");
        if !printable || *template {
            census.unresolved_layers.insert(layer.into());
            census.unresolved.push(format!("{}: nonprinting layer requires direct PDF print-state verification", node.id));
            return;
        }
    }
    // PDF print-state and viewer visibility require reconciliation by the final interpreter.
    if !visible {
        census.unresolved_layers.insert(layer.into());
        census.unresolved.push(format!("{}: optional-content print state needs verification", node.id));
    }
    if matches!(node.kind, NodeKind::Image(_)) {
        census.image_objects += 1;
    }
    let bounds = node.geometric_bounds_with(symbols).map(|b| [b.x0 * 25.4 / 72.0, b.y0 * 25.4 / 72.0, b.x1 * 25.4 / 72.0, b.y1 * 25.4 / 72.0]);
    match &node.kind {
        NodeKind::Layer { .. } | NodeKind::Group { .. } | NodeKind::Compound { .. } | NodeKind::Path { guide: false, .. } => {}
        NodeKind::Path { guide: true, .. } => return,
        NodeKind::Image(_)
        | NodeKind::Mesh(_)
        | NodeKind::Text(_)
        | NodeKind::SymbolInstance { .. }
        | NodeKind::Blend { .. }
        | NodeKind::Envelope { .. }
        | NodeKind::Repeat(_)
        | NodeKind::PlacedDocument(_)
        | NodeKind::Intertwine { .. } => {
            census.unresolved_layers.insert(layer.into());
            census.unaccounted_objects.push(format!("node-{}-{}", node.id, node.kind_label()));
            census.unresolved.push(format!("{}: {} requires direct composited content census", node.id, node.kind_label()));
        }
    }
    if node.opacity != 1.0 || node.blend != BlendMode::Normal || node.extras().mask.is_some() || !node.appearance.effects.is_empty() {
        census.unresolved_layers.insert(layer.into());
        census.unresolved.push(format!("{}: transparency/mask/effect requires composited coverage", node.id));
    }
    for item in &node.appearance.items {
        if !item.visible() || item.paint().is_none() {
            continue;
        }
        if census.paints.len() >= 50_000 {
            census.budget_exhausted = true;
            return;
        }
        let (ink, process, tint) = match item.paint() {
            Paint::Solid { swatch, tint, .. } => {
                let spot = swatch.as_ref().filter(|name| doc.swatch(name).is_some_and(|s| s.spot)).cloned();
                let process = spot.is_none();
                (spot, process, Some(*tint))
            }
            _ => {
                census.unresolved_layers.insert(layer.into());
                census.unresolved.push(format!("{}: non-solid paint needs composited census", node.id));
                (None, false, None)
            }
        };
        let mut geometry_mm = None;
        let mut page = None;
        let mut geometry_error_mm = 0.00001;
        if item.opacity() == 1.0
            && item.blend() == BlendMode::Normal
            && item.effects().is_empty()
            && let Some(path) = node.stroke_path()
        {
            let rule = match &node.kind {
                NodeKind::Path { rule, .. } | NodeKind::Compound { rule, .. } => *rule,
                _ => vectorcraft_geom::FillRule::NonZero,
            };
            let coverage = match item {
                vectorcraft_doc::AppearanceItem::Fill(_) => filled(&PathData::from_bezpath(&path), rule),
                vectorcraft_doc::AppearanceItem::Stroke(stroke) => {
                    // The shared stroker clamps to 1e-4 pt. Keep that numerical envelope
                    // explicitly in both source construction and observed measurements.
                    geometry_error_mm += 0.0001 * 25.4 / 72.0;
                    stroke_coverage(&path, stroke)
                }
            };
            let geometry = coverage.and_then(|p| match state.clip {
                Some(clip) => intersect(&p, clip),
                None => Ok(p),
            });
            match geometry {
                Ok(path) => {
                    let bounds = path.to_bezpath().bounding_box();
                    let pages: Vec<_> = doc.artboards.iter().enumerate().filter(|(_, a)| a.rect.contains_rect(bounds)).collect();
                    if let [(n, board)] = pages.as_slice() {
                        geometry_mm = Some(path.transformed(Affine::scale(25.4 / 72.0) * Affine::translate((-board.rect.x0, -board.rect.y0))));
                        page = Some(*n);
                    }
                }
                Err(error) => {
                    census.unresolved_layers.insert(layer.into());
                    census.unresolved.push(format!("{}: fill/clip coverage cannot be resolved: {error}", node.id));
                }
            }
        }
        if geometry_mm.is_none() {
            census.unresolved_layers.insert(layer.into());
            census.unresolved.push(format!("{}: printable geometry requires composited measurement", node.id));
        }
        census.paints.push(PaintedObject {
            id: node.id.0,
            layer: layer.into(),
            ink,
            process,
            overprint: item.overprint(),
            overprint_zero: match item {
                vectorcraft_doc::AppearanceItem::Fill(f) => f.overprint_zero,
                vectorcraft_doc::AppearanceItem::Stroke(s) => s.overprint_zero,
            },
            tint,
            bounds_mm: bounds,
            kind: node.kind_label().into(),
            order: census.paints.len(),
            source_space: None,
            page,
            geometry_mm,
            geometry_error_mm,
            paint: serde_json::to_value(item.paint()).unwrap_or(Value::Null),
        });
    }
    if let Some(children) = node.children() {
        let mut clip = None;
        if node.clips() {
            let resolved = (|| -> Result<PathData, String> {
                let mask = children.first().ok_or("Clipping group has no clip object")?;
                let parts = mask
                    .clip_shapes(None)
                    .into_iter()
                    .map(|(path, rule)| filled(&PathData::from_bezpath(&path), rule))
                    .collect::<Result<Vec<_>, _>>()?;
                let coverage = crate::program::union(parts)?;
                if let Some(parent) = state.clip { intersect(&coverage, parent) } else { Ok(coverage) }
            })();
            match resolved {
                Ok(coverage) => clip = Some(coverage),
                Err(error) => {
                    census.unresolved_layers.insert(layer.into());
                    census.unresolved.push(format!("{}: clip coverage unresolved: {error}", node.id));
                    return;
                }
            }
        }
        let active = clip.as_ref().or(state.clip);
        if node.clips()
            && let Some(mask) = children.first()
            && let Some(fill) = mask.clip_paint().fill
        {
            walk(doc, symbols, &fill, layer, Traversal { visible, depth: state.depth + 1, clip: active }, census);
        }
        for child in children.iter().skip(usize::from(node.clips())) {
            if census.budget_exhausted {
                break;
            }
            walk(doc, symbols, child, layer, Traversal { visible, depth: state.depth + 1, clip: active }, census);
        }
        if node.clips()
            && let Some(mask) = children.first()
            && let Some(stroke) = mask.clip_paint().stroke
        {
            walk(doc, symbols, &stroke, layer, Traversal { visible, depth: state.depth + 1, clip: state.clip }, census);
        }
    }
}

pub fn census(doc: &Document) -> Census {
    let mut c = Census {
        paint_order_verified: false,
        raw_audit: None,
        raw_audit_error: None,
        image_objects: 0,
        paint_sources: vec![],
        paints: vec![],
        unresolved: vec![],
        unresolved_layers: Default::default(),
        pages: doc.artboards.len(),
        page_sizes_mm: doc.artboards.iter().map(|a| [a.rect.width() * 25.4 / 72.0, a.rect.height() * 25.4 / 72.0]).collect(),
        budget_exhausted: false,
        object_count: 0,
        unaccounted_objects: vec![],
    };
    let symbols = doc.symbol_boxes();
    for layer in &doc.layers {
        if c.budget_exhausted {
            break;
        }
        walk(doc, &symbols, layer, "", Traversal { visible: true, depth: 0, clip: None }, &mut c);
    }
    c
}

/// The checker explicitly requests embedded glyph outlines. That conversion notice
/// is retained as evidence, while every other importer warning remains unresolved.
pub(crate) fn import_messages(warnings: Vec<String>) -> (Vec<String>, Vec<String>) {
    warnings.into_iter().partition(|w| w != "text was converted to outlines")
}

pub fn check(request: &Request, bytes: &[u8]) -> Result<Report, String> {
    let mut report = begin(request)?;
    preparation::check_inputs(request, &mut report)?;
    if crate::digest(&request.recipe)? != request.identity.recipe_sha256 {
        return Ok(report);
    }
    let source_plan = crate::attribution::check(request, &mut report)?;
    let hash = sha256(bytes);
    let evidence = vec![Evidence {
        artifact_sha256: hash.clone(),
        producer: "craft-pdf/public-content/checks-1".into(),
        detail: "Fresh import of exact supplied PDF bytes; native embedded document ignored".into(),
    }];
    if hash != request.identity.pdf_sha256 {
        report.record("global/export", Status::Incomplete, "PDF bytes differ from the requested identity", json!(hash), evidence)?;
        return Ok(report);
    }
    // Byte limit is an engineering resource bound, never a skipped-page success.
    if bytes.len() > 256 * 1024 * 1024 {
        report.record("global/integrity", Status::Incomplete, "PDF exceeds this worker's 256 MiB input budget", json!(bytes.len()), evidence)?;
        return Ok(report);
    }
    let imported = match vectorcraft_pdf::import_with_report(
        bytes,
        &vectorcraft_pdf::ImportOptions { reject_depth_loss: true, text_as: vectorcraft_pdf::TextAs::Outlines, ..Default::default() },
    ) {
        Ok(imported) => imported,
        Err(error) => {
            report.record("global/integrity", Status::Incomplete, format!("PDF interpretation failed: {error}"), Value::Null, evidence)?;
            return Ok(report);
        }
    };
    let mut c = census(&imported.document);
    let (warnings, notices) = import_messages(imported.warnings);
    let import_complete = warnings.is_empty();
    c.unresolved.extend(warnings);
    // Ordering is required for overlapping paint, but an ordering gap does not
    // erase independently measurable geometry or the existing diagnostics.
    let _ = crate::ordering::try_reconcile(bytes, &mut c);
    report.record(
        "global/integrity",
        Status::Incomplete,
        "Public PDF parsed; complete printable-content accounting requires independent census reconciliation",
        json!({"pages":c.pages,"paint_count":c.paints.len(),"budget_exhausted":c.budget_exhausted,"notices":notices,
            "decoded_image_objects":c.image_objects,"raw_paint_audit":c.raw_audit,"raw_paint_audit_error":c.raw_audit_error,
            "source_space_count":c.paint_sources.len(),"source_spaces_sample":c.paint_sources.iter().take(32).collect::<Vec<_>>(),
            "unresolved_count":c.unresolved.len(),"unresolved_sample":c.unresolved.iter().take(64).collect::<Vec<_>>()}),
        evidence.clone(),
    )?;
    // All file links already exist in the denominator. No candidate filtering changes it.
    for link in request.recipe.file_links.iter().filter(|l| request.garment.matches(&l.garment)) {
        if report.findings.iter().any(|f| f.obligation.key == format!("file/{}/contribution", link.key) && f.status == Status::NotApplicable) {
            continue;
        }
        let candidates: Vec<_> = c.paints.iter().filter(|p| p.layer == link.pdf_layer).collect();
        let matching: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|p| match link.name_mode.as_str() {
                "exact" => p.ink.as_ref() == link.separation_name.as_ref() && p.ink.is_some(),
                _ => false, // Retained source inks need the prepared assignments, not guessed CMYK.
            })
            .collect();
        if !import_complete || c.budget_exhausted || c.unresolved_layers.contains(&link.pdf_layer) {
            continue;
        }
        if !matching.is_empty() {
            report.record(
                &format!("file/{}/layer", link.key),
                Status::Pass,
                "Required PDF layer contains the exact named ink",
                json!(candidates.len()),
                evidence.clone(),
            )?;
        }
        if matching.is_empty() {
            continue;
        }
        report.record(
            &format!("file/{}/identity", link.key),
            Status::Pass,
            "Exact named ink is painted in the required layer",
            json!({"count":matching.len(),"object_sample":matching.iter().take(64).map(|p|p.id).collect::<Vec<_>>()}),
            evidence.clone(),
        )?;
        let wrong: Vec<_> = matching.iter().filter(|p| p.overprint != link.overprint).collect();
        report.record(
            &format!("file/{}/overprint", link.key),
            if wrong.is_empty() { Status::Pass } else { Status::Fail },
            "Paint overprint flags checked; effective knockout/compositing remains a separate requirement",
            json!({"expected":link.overprint,"wrong_count":wrong.len(),"wrong_object_sample":wrong.iter().take(64).map(|p|p.id).collect::<Vec<_>>()}),
            evidence.clone(),
        )?;
    }
    if import_complete
        && !c.budget_exhausted
        && let Some(plan) = source_plan
    {
        crate::measure::check(request, &plan, &c, &mut report)?;
    }
    crate::ordering::check(request, bytes, &c, &mut report)?;
    crate::specification::check(request, bytes, &mut report)?;
    Ok(report)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    // Original synthetic PDF written directly from ISO 32000 syntax, independently of craft-pdf.
    pub(crate) fn pdf(overprint: bool) -> Vec<u8> {
        fixture(overprint, "Exact#20ink", "", None)
    }
    pub(crate) fn fixture(overprint: bool, name: &str, extra: &str, native: Option<&str>) -> Vec<u8> {
        fixture_mode(overprint, 1, name, extra, native)
    }
    pub(crate) fn fixture_mode(overprint: bool, mode: u8, name: &str, extra: &str, native: Option<&str>) -> Vec<u8> {
        let stream = format!("/OC /Layer BDC 1 0 0 0 k 0 0 50 50 re f /GS gs /Ink cs 1 scn 10 10 20 20 re f {extra} EMC");
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R] /D << /Order [6 0 R] /ON [6 0 R] >> >> >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /ColorSpace << /Ink [/Separation /{name} /DeviceCMYK 5 0 R] >> /Properties << /Layer 6 0 R >> /ExtGState << /GS 7 0 R >> >> /Contents 4 0 R >>"
            ),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>".into(),
            "<< /Type /OCG /Name (Layer) >>".into(),
            format!("<< /Type /ExtGState /op {overprint} /OP {overprint} /OPM {mode} >>"),
        ];
        if let Some(native) = native {
            objects[0]="<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [(vectorcraft-editing.vectorcraft) 8 0 R] >> >> /OCProperties << /OCGs [6 0 R] /D << /Order [6 0 R] /ON [6 0 R] >> >> >>".into();
            objects.push("<< /Type /Filespec /F (vectorcraft-editing.vectorcraft) /EF << /F 9 0 R >> >>".into());
            objects.push(format!("<< /Type /EmbeddedFile /Length {} >>\nstream\n{native}\nendstream", native.len()));
        }
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![0];
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
        for offset in offsets.iter().skip(1) {
            out.extend(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len()).as_bytes());
        out
    }
    #[test]
    fn exact_pdf_detects_wrong_overprint_but_never_awards_overall_pass() {
        for (overprint, expected) in [(true, Status::Pass), (false, Status::Fail)] {
            let bytes = pdf(overprint);
            let mut request = crate::tests::request();
            request.identity.pdf_sha256 = sha256(&bytes);
            request.identity.recipe_sha256 = crate::digest(&request.recipe).unwrap();
            let report = check(&request, &bytes).unwrap();
            let finding = report.findings.iter().find(|f| f.obligation.key == "file/output/overprint").unwrap();
            assert_eq!(finding.status, expected, "{}", serde_json::to_string_pretty(&report).unwrap());
            assert_ne!(report.status, Status::Pass);
            assert!(report.coverage.incomplete > 0);
        }
    }
    #[test]
    fn wrong_bytes_and_corrupt_pdf_keep_full_denominator() {
        let mut r = crate::tests::request();
        r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
        let n = crate::enumerate(&r).unwrap().len();
        assert_eq!(check(&r, b"bad").unwrap().coverage.total, n);
        r.identity.pdf_sha256 = sha256(b"bad");
        assert_eq!(check(&r, b"bad").unwrap().coverage.total, n);
    }
    #[test]
    fn fixture_has_decoded_exact_spot_name() {
        let doc = vectorcraft_pdf::import_with_report(&pdf(true), &Default::default()).unwrap().document;
        let c = census(&doc);
        assert!(c.paints.iter().any(|p| p.ink.as_deref() == Some("Exact ink")));
        assert!(!c.paints.iter().any(|p| p.ink.as_deref() == Some("Exact#20ink")));
    }
    fn report_for(bytes: &[u8]) -> Report {
        let mut request = crate::tests::request();
        request.identity.pdf_sha256 = sha256(bytes);
        request.identity.recipe_sha256 = crate::digest(&request.recipe).unwrap();
        check(&request, bytes).unwrap()
    }
    #[test]
    fn case_and_trailing_space_are_distinct_production_inks() {
        for name in ["exact#20ink", "Exact#20Ink", "Exact#20ink#20"] {
            let report = report_for(&fixture(true, name, "", None));
            assert_ne!(report.findings.iter().find(|f| f.obligation.key == "file/output/identity").unwrap().status, Status::Pass, "{name}");
        }
    }
    #[test]
    fn embedded_editing_payload_cannot_replace_public_paint() {
        let good = vectorcraft_pdf::import_with_report(&pdf(true), &Default::default()).unwrap().document;
        let native = serde_json::to_string(&json!({"format":"vectorcraft","version":3,"document":good,"images":{}})).unwrap();
        let bytes = fixture(false, "Exact#20ink", "", Some(&native));
        let imported = vectorcraft_pdf::import_with_report(&bytes, &Default::default()).unwrap();
        assert_eq!(imported.native.unwrap().data, native.as_bytes());
        let report = report_for(&bytes);
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "file/output/overprint").unwrap().status, Status::Fail);
    }
    #[test]
    fn clipped_content_is_accounted_before_overprint_flags_pass() {
        let bytes = fixture(true, "Exact#20ink", "q 0 0 5 5 re W n 0 0 15 15 re f Q", None);
        let report = report_for(&bytes);
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "file/output/overprint").unwrap().status, Status::Pass);
    }
    #[test]
    fn nested_evenodd_clipping_retains_the_hole_and_actual_coverage() {
        let bytes = fixture(true, "Exact#20ink", "q 0 0 40 40 re 5 5 30 30 re W* n q 0 0 20 40 re W n 0 0 40 40 re f Q Q", None);
        let doc = vectorcraft_pdf::import(&bytes).unwrap();
        let c = census(&doc);
        assert!(c.unresolved.is_empty(), "{:?}", c.unresolved);
        assert_eq!(c.paints.len(), 3);
        let shape = c.paints.last().unwrap().geometry_mm.as_ref().unwrap().to_bezpath();
        let point = |x: f64, y: f64| vectorcraft_geom::Point::new(x * 25.4 / 72.0, (100.0 - y) * 25.4 / 72.0);
        assert_ne!(shape.winding(point(2.0, 2.0)), 0);
        assert_eq!(shape.winding(point(10.0, 10.0)), 0);
        assert_eq!(shape.winding(point(25.0, 2.0)), 0);
    }
}
