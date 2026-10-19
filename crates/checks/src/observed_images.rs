//! A process-only image prelude cannot add exact technical inks or erase later
//! vectors. This proof resolves only that isolation, never image coverage.
use crate::{Request, pdf::Census};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use vectorcraft_geom::Affine;

pub(crate) struct Prelude {
    census: Census,
    links: BTreeSet<String>,
    unused_duplicate_layers: usize,
}
impl Prelude {
    pub(crate) fn census(&self) -> &Census {
        &self.census
    }
    pub(crate) fn affects(&self, key: &str) -> bool {
        self.links.contains(key)
    }
    pub(crate) fn summary(&self) -> serde_json::Value {
        serde_json::json!({"proof":"process-only images before all vectors; technical ink isolation only", "image_count":self.census.image_objects,"affected_source_links":self.links,"unused_duplicate_layer_declarations":self.unused_duplicate_layers,"unresolved":"image coverage, routing, flags, mixed order and source fidelity remain unresolved"})
    }
    pub(crate) fn new(r: &Request, bytes: &[u8], doc: &vectorcraft_doc::Document, warnings: &[String], general: &Census) -> Result<Self, String> {
        let mut order = vectorcraft_pdf::print_order(bytes).map_err(|e| e.to_string())?;
        let unused_duplicate_layers = reconcile_unused_layers(&mut order)?;
        if unused_duplicate_layers > 0 {
            reconcile_native_layers(doc)?;
        }
        let count = order.images.iter().map(Vec::len).sum::<usize>();
        if count == 0 || count > 4096 || general.raw_audit_error.is_some() || general.raw_audit.as_ref().is_none_or(|a| a.image_use_count != count) {
            return Err("Raw image inventory is missing or inconsistent".into());
        }
        let info = vectorcraft_pdf::production_info(bytes).map_err(|e| e.to_string())?;
        let mut census = crate::pdf::census_for_raw_image_prediction(doc);
        if census.page_sizes_mm.len() != info.pages.len()
            || info.pages.len() != order.images.len()
            || census.page_sizes_mm.iter().zip(&info.pages).any(|(size, page)| {
                let w = page.crop_mm[2].min(page.media_mm[2]) - page.crop_mm[0].max(page.media_mm[0]);
                let h = page.crop_mm[3].min(page.media_mm[3]) - page.crop_mm[1].max(page.media_mm[1]);
                let expected = if matches!(page.rotation, 90 | 270) { [h, w] } else { [w, h] };
                size.iter().zip(expected).any(|(a, b)| !a.is_finite() || !b.is_finite() || b <= 0.0 || (*a - b).abs() > 0.000001)
            })
        {
            return Err("Native and raw physical page frames differ".into());
        }
        if warnings.iter().any(|w| w != "original PDF page boxes are not retained for a rotated, scaled-unit or artwork-bounds import") {
            return Err("Unresolved import warnings prevent technical-vector isolation".into());
        }
        crate::ordering::reconcile_paths_for_images(&order, &mut census)?;
        if !census.unaccounted_objects.is_empty() || !census.unresolved_layers.is_empty() {
            return Err("Unaccounted native image/vector contexts".into());
        }
        let active: Vec<_> = r.recipe.file_links.iter().filter(|l| r.garment.matches(&l.garment)).collect();
        let technical: BTreeSet<_> = active.iter().filter(|l| l.name_mode == "exact").filter_map(|l| l.separation_name.as_deref()).collect();
        // Unqualified process conversion can introduce a different process
        // component from the decoded source samples. Reserved output names
        // therefore cannot be isolated as exact technical plates by this proof.
        if technical.iter().any(|n| ["Cyan", "Magenta", "Yellow", "Black", "All", "None"].contains(n)) {
            return Err("Process or reserved technical names need independent image colour conversion".into());
        }
        let mut contexts: BTreeMap<usize, VecDeque<_>> = BTreeMap::new();
        for image in &census.prediction_images {
            let layer = order.layers.iter().position(|n| n == &image.layer).ok_or("Image layer has no raw identity")?;
            contexts.entry(layer).or_default().push_back(image);
        }
        let mut links = BTreeSet::new();
        for (page_index, (page, images)) in info.pages.iter().zip(&order.images).enumerate() {
            let origin = census.page_origins_mm.get(page_index).ok_or("Image page origin missing")?;
            for image in images {
                let d = &image.description;
                if image.before_path != 0
                    || image.compositing_unresolved
                    || image.sample_error.is_some()
                    || image.overprint
                    || d.optional_content
                    || d.external_data
                    || d.interpolate
                    || d.stencil
                    || !d.issues.is_empty()
                    || !matches!(d.colour_space.as_str(), "DeviceGray" | "DeviceCMYK" | "Separation" | "DeviceN")
                {
                    return Err("Image is not a supported process-only prelude".into());
                }
                let samples = image.samples.as_ref().ok_or("Image samples are unresolved")?;
                if samples.width == 0
                    || samples.height == 0
                    || samples.colourants.is_empty()
                    || samples
                        .colourants
                        .iter()
                        .any(|n| !["Cyan", "Magenta", "Yellow", "Black"].contains(&n.as_str()) || technical.contains(n.as_str()))
                    || !samples.minimum_alpha.is_finite()
                    || !samples.maximum_alpha.is_finite()
                    || samples.minimum_alpha < 0.0
                    || samples.maximum_alpha > 1.0
                    || samples.minimum_alpha > samples.maximum_alpha
                {
                    return Err("Image colourants overlap technical inks or have unresolved samples".into());
                }
                let layer = image.layer.ok_or("Image has no exact layer identity")?;
                let name = order.layers.get(layer).ok_or("Image layer index missing")?;
                let routes: Vec<_> = active.iter().filter(|l| crate::routes::layer_matches(&l.pdf_layer, name)).collect();
                if routes.len() != 1 || routes.first().is_none_or(|l| l.name_mode != "source") {
                    return Err("Image needs an unambiguous retained-source route".into());
                }
                let route = routes.first().ok_or("Image source route missing")?;
                links.insert(route.key.clone());
                let context = contexts.get_mut(&layer).and_then(VecDeque::pop_front).ok_or("Native image context missing")?;
                let transform = page_transform(page)? * Affine::new(image.matrix.ok_or("Image matrix missing")?);
                if !transform.determinant().is_finite() || transform.determinant() == 0.0 {
                    return Err("Image placement is singular".into());
                }
                let expected = Affine::translate((origin[0], origin[1])) * transform;
                if context.dimensions != [samples.width, samples.height]
                    || d.dimensions != Some(context.dimensions)
                    || context
                        .transform_mm
                        .as_coeffs()
                        .into_iter()
                        .zip(expected.as_coeffs())
                        .any(|(a, b)| !a.is_finite() || !b.is_finite() || (a - b).abs() > 0.000001)
                    || image.clip_context && context.clip_mm.is_none()
                {
                    return Err("Native/raw image dimensions, placement or clipping context differ".into());
                }
            }
        }
        if contexts.values().any(|q| !q.is_empty()) {
            return Err("Unmatched native images".into());
        }
        // Retain the real diagnostic, including its images and unresolved issue.
        // Only this typed proof can select the narrower observed-vector path.
        census.raw_audit = general.raw_audit.clone();
        Ok(Self { census, links, unused_duplicate_layers })
    }
}

// Form-owned /OC can disagree with the native interpreter's enclosing marked
// scope. Do not let equal names hide two populated native layer identities and
// thereby reorder their paints when the native per-layer queues are joined.
fn reconcile_native_layers(doc: &vectorcraft_doc::Document) -> Result<(), String> {
    use vectorcraft_doc::NodeKind;
    let mut stack: Vec<_> = doc.layers.iter().map(|n| (n.as_ref(), None, 0usize)).collect();
    let mut used = BTreeMap::new();
    let mut count = 0usize;
    while let Some((node, mut layer, depth)) = stack.pop() {
        count += 1;
        if count > 50_000 || depth > 256 {
            return Err("Native layer identity census exceeds resource budget".into());
        }
        if matches!(node.kind, NodeKind::Layer { .. }) {
            layer = Some((node.name.as_deref().unwrap_or(""), node.id));
        }
        if (matches!(node.kind, NodeKind::Path { .. } | NodeKind::Image(_) | NodeKind::Compound { .. })
            || node.appearance.items.iter().any(|i| !i.paint().is_none()))
            && let Some((name, id)) = layer
            && used.insert(name, id).is_some_and(|previous| previous != id)
        {
            return Err("Multiple native painted layer identities share the same name".into());
        }
        if let Some(children) = node.children() {
            if count.saturating_add(stack.len()).saturating_add(children.len()) > 50_000 {
                return Err("Native layer identity census exceeds resource budget".into());
            }
            stack.extend(children.iter().map(|n| (n.as_ref(), layer, depth + 1)));
        }
    }
    Ok(())
}

// A declared name need not identify a unique OCG when all but one of its
// identities have no raw paint events. Canonicalize only this private proof's
// lookup dictionary; preserve the original general audit and its diagnostic.
// Both paths and images count as use, including clipped and zero-tint paint.
fn reconcile_unused_layers(order: &mut vectorcraft_pdf::PrintOrder) -> Result<usize, String> {
    const ISSUE: &str = "Duplicate PDF layer names cannot establish unique routing";
    if !order.issues.contains(ISSUE) {
        return Ok(0);
    }
    let mut used: BTreeMap<&str, usize> = BTreeMap::new();
    for index in order.pages.iter().flatten().copied().chain(order.images.iter().flatten().map(|i| i.layer)).flatten() {
        let name = order.layers.get(index).ok_or("Raw paint layer index is unresolved")?;
        if used.insert(name.as_str(), index).is_some_and(|previous| previous != index) {
            return Err("Multiple painted layer identities share the same name".into());
        }
    }
    let mut names = BTreeMap::new();
    let mut layers = Vec::new();
    let mut mapping = Vec::new();
    for name in &order.layers {
        let index = *names.entry(name.clone()).or_insert_with(|| {
            let index = layers.len();
            layers.push(name.clone());
            index
        });
        mapping.push(index);
    }
    for index in order.pages.iter_mut().flatten().chain(order.images.iter_mut().flatten().map(|i| &mut i.layer)).flatten() {
        *index = *mapping.get(*index).ok_or("Raw layer identity cannot be reconciled")?;
    }
    let removed = order.layers.len() - layers.len();
    order.layers = layers;
    order.issues.remove(ISSUE);
    Ok(removed)
}
fn page_transform(page: &vectorcraft_pdf::ProductionPage) -> Result<Affine, String> {
    let [mx0, my0, mx1, my1] = page.media_mm;
    let [cx0, cy0, cx1, cy1] = page.crop_mm;
    let [x0, y0, x1, y1] = [mx0.max(cx0), my0.max(cy0), mx1.min(cx1), my1.min(cy1)];
    if x0 >= x1 || y0 >= y1 {
        return Err("Empty physical image crop".into());
    }
    Ok(match page.rotation {
        0 => Affine::new([1.0, 0.0, 0.0, -1.0, -x0, y1]),
        90 => Affine::new([0.0, 1.0, 1.0, 0.0, -y0, -x0]),
        180 => Affine::new([-1.0, 0.0, 0.0, 1.0, x1, -y0]),
        270 => Affine::new([0.0, -1.0, -1.0, 0.0, y1, x1]),
        _ => return Err("Unsupported image page rotation".into()),
    } * Affine::scale(page.user_unit * 25.4 / 72.0))
}
