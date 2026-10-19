//! A process-only image prelude cannot add exact technical inks or erase later
//! vectors. This proof resolves only that isolation, never image coverage.
use crate::{Request, pdf::Census};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use vectorcraft_geom::Affine;

pub(crate) struct Prelude {
    census: Census,
    links: BTreeSet<String>,
}
impl Prelude {
    pub(crate) fn census(&self) -> &Census {
        &self.census
    }
    pub(crate) fn affects(&self, key: &str) -> bool {
        self.links.contains(key)
    }
    pub(crate) fn summary(&self) -> serde_json::Value {
        serde_json::json!({"proof":"process-only images before all vectors; technical ink isolation only", "image_count":self.census.image_objects,"affected_source_links":self.links,"unresolved":"image coverage, routing, flags, mixed order and source fidelity remain unresolved"})
    }
    pub(crate) fn new(r: &Request, bytes: &[u8], doc: &vectorcraft_doc::Document, warnings: &[String], general: &Census) -> Result<Self, String> {
        let order = vectorcraft_pdf::print_order(bytes).map_err(|e| e.to_string())?;
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
                let routes: Vec<_> = active.iter().filter(|l| l.pdf_layer == *name).collect();
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
        Ok(Self { census, links })
    }
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
