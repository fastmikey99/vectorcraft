//! Raw image and reconciled vector PDF prediction, independently reconciled to tiffsep planes.
//! Reject unsupported contexts before comparing. This is a feature-specific measurement,
//! not runtime qualification, a prepared hard mask, or proof of artwork fidelity.
use crate::{
    Status,
    independent::Render,
    raster::{Comparison, PlaneComparison},
};
use std::collections::BTreeSet;
use vectorcraft_geom::{Affine, Point};

struct ImagePaint<'a> {
    clip: Option<crate::geometry::Boundary>,
    transform: Affine,
    samples: &'a vectorcraft_pdf::PrintImageSamples,
    inverse: Affine,
    margin: [f64; 2],
}
fn paints<'a>(
    page: &vectorcraft_pdf::ProductionPage,
    images: &'a [vectorcraft_pdf::PrintImageUse],
    uncertainty: f64,
) -> Result<Vec<ImagePaint<'a>>, String> {
    let [mx0, my0, mx1, my1] = page.media_mm;
    let [cx0, cy0, cx1, cy1] = page.crop_mm;
    let [x0, y0, x1, y1] = [mx0.max(cx0), my0.max(cy0), mx1.min(cx1), my1.min(cy1)];
    if x0 >= x1 || y0 >= y1 {
        return Err("Empty physical image page crop".into());
    }
    let page_transform = match page.rotation {
        0 => Affine::new([1.0, 0.0, 0.0, -1.0, -x0, y1]),
        90 => Affine::new([0.0, 1.0, 1.0, 0.0, -y0, -x0]),
        180 => Affine::new([-1.0, 0.0, 0.0, 1.0, x1, -y0]),
        270 => Affine::new([0.0, -1.0, -1.0, 0.0, y1, x1]),
        _ => return Err("Unsupported page rotation".into()),
    } * Affine::scale(page.user_unit * 25.4 / 72.0);
    images
        .iter()
        .map(|image| {
            if image.overprint || image.compositing_unresolved || image.description.optional_content || image.description.interpolate {
                return Err("Image overprint, graphics state, optional content or interpolation requires broader compositing qualification".into());
            }
            let samples = image.samples.as_ref().ok_or_else(|| image.sample_error.clone().unwrap_or("Image samples missing".into()))?;
            let transform = page_transform * Affine::new(image.matrix.ok_or("Image transform missing")?);
            if !transform.determinant().is_finite() || transform.determinant() == 0.0 {
                return Err("Singular image transform".into());
            }
            let inverse = transform.inverse();
            let [a, b, c, d, _, _] = inverse.as_coeffs();
            let margin = [uncertainty * a.hypot(c), uncertainty * b.hypot(d)];
            if margin.iter().any(|n| !n.is_finite()) || margin[0] * f64::from(samples.width) >= 0.5 || margin[1] * f64::from(samples.height) >= 0.5 {
                return Err("Independent resolution cannot resolve image sample boundaries".into());
            }
            Ok(ImagePaint { samples, inverse, margin, clip: None, transform })
        })
        .collect()
}
impl ImagePaint<'_> {
    fn sample(&self, p: Point, name: &str) -> Result<Option<(f64, f64)>, ()> {
        let p = self.inverse * p;
        let [u, v] = [p.x, 1.0 - p.y];
        if u < -self.margin[0] || u > 1.0 + self.margin[0] || v < -self.margin[1] || v > 1.0 + self.margin[1] {
            return Ok(None);
        }
        if u < self.margin[0] || u > 1.0 - self.margin[0] || v < self.margin[1] || v > 1.0 - self.margin[1] {
            return Err(());
        }
        let x = u * f64::from(self.samples.width);
        let y = v * f64::from(self.samples.height);
        if x.fract().min(1.0 - x.fract()) < self.margin[0] * f64::from(self.samples.width)
            || y.fract().min(1.0 - y.fract()) < self.margin[1] * f64::from(self.samples.height)
        {
            return Err(());
        }
        let at = (y.floor() as usize) * self.samples.width as usize + x.floor() as usize;
        let alpha = self.samples.alpha(at).map_err(|_| ())?;
        let tone = match self.samples.colourants.iter().position(|n| n == name) {
            Some(channel) => self.samples.tone(at, channel).map_err(|_| ())?,
            None => 0.0,
        };
        Ok(Some((tone, alpha)))
    }
}
struct VectorPaint {
    boundary: crate::geometry::Boundary,
    inks: std::collections::BTreeMap<String, f64>,
    overprint: bool,
    skip_zero_process: bool,
}
enum PaintEvent<'a> {
    Image(ImagePaint<'a>),
    Vector(VectorPaint),
}
type Sample = Result<Option<(f64, f64)>, ()>;
impl PaintEvent<'_> {
    fn sample(&self, point: Point, name: &str, uncertainty: f64, budget: &mut usize) -> Result<Sample, String> {
        match self {
            Self::Image(image) => {
                if let Some(clip) = &image.clip {
                    if clip.near_boundary(point, uncertainty, budget)? {
                        return Ok(Err(()));
                    }
                    if !clip.contains_bounded(point, vectorcraft_geom::FillRule::NonZero, budget)? {
                        return Ok(Ok(None));
                    }
                }
                Ok(image.sample(point, name))
            }
            Self::Vector(paint) => {
                if paint.boundary.near_boundary(point, uncertainty, budget)? {
                    return Ok(Err(()));
                }
                if !paint.boundary.contains_bounded(point, vectorcraft_geom::FillRule::NonZero, budget)? {
                    return Ok(Ok(None));
                }
                Ok(Ok(crate::tone::overpaint(&paint.inks, name, paint.overprint, paint.skip_zero_process).map(|tone| (tone, 1.0))))
            }
        }
    }
}
fn vector_paint(paint: &crate::pdf::PaintedObject, census: &crate::pdf::Census) -> Result<VectorPaint, String> {
    let path = paint.geometry_mm.as_ref().ok_or("Mixed vector paint has no coverage")?;
    let source = paint.source_space.and_then(|i| census.paint_sources.get(i)).ok_or("Mixed vector source space missing")?;
    let inks = crate::raster::source_tones(paint, source)?;
    if inks.values().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
        return Err("Mixed vector source tone invalid".into());
    }
    Ok(VectorPaint {
        boundary: crate::geometry::Boundary::new(path, vectorcraft_geom::FillRule::NonZero, 0.000001)?,
        inks,
        overprint: paint.overprint,
        skip_zero_process: crate::tone::skip_zero_process(paint, source),
    })
}
fn reconcile_image_clip(
    raw: &vectorcraft_pdf::PrintImageUse,
    context: &crate::pdf::PredictionImage,
    origin: &[f64; 2],
    paint: &mut ImagePaint<'_>,
) -> Result<(), String> {
    let expected = Affine::translate((origin[0], origin[1])) * paint.transform;
    if context.dimensions != [paint.samples.width, paint.samples.height]
        || context
            .transform_mm
            .as_coeffs()
            .into_iter()
            .zip(expected.as_coeffs())
            .any(|(a, b)| !a.is_finite() || !b.is_finite() || (a - b).abs() > 0.000001)
    {
        return Err("Native/raw image dimensions, placement or order differ".into());
    }
    if raw.clip_context && context.clip_mm.is_none() {
        return Err("Raw image clipping has no reconciled native boundary".into());
    }
    paint.clip = context
        .clip_mm
        .as_ref()
        .map(|p| {
            crate::geometry::Boundary::new(&p.transformed(Affine::translate((-origin[0], -origin[1]))), vectorcraft_geom::FillRule::NonZero, 0.000001)
        })
        .transpose()?;
    Ok(())
}
pub(crate) fn compare(bytes: &[u8], render: &Render, tolerance: Option<f64>) -> Result<Comparison, String> {
    if !render.pixel_pitch_mm.is_finite() || render.pixel_pitch_mm <= 0.0 {
        return Err("Invalid independent image pixel pitch".into());
    }
    let uncertainty = render.pixel_pitch_mm * 2.0f64.sqrt() + 0.000002;
    let incomplete = |reason: String| Comparison { status: Status::Incomplete, uncertainty_mm: uncertainty, planes: vec![], issues: vec![reason] };
    if render.pdf_sha256 != crate::sha256(bytes) {
        return Err("Image comparison PDF identity mismatch".into());
    }
    if !render.diagnostics.is_empty() || tolerance.is_none_or(|t| !t.is_finite() || t < uncertainty) {
        return Ok(incomplete("Interpreter diagnostics or unresolved geometric acceptance".into()));
    }
    let order = vectorcraft_pdf::print_order(bytes).map_err(|e| e.to_string())?;
    if order.images.iter().all(Vec::is_empty)
        || order.issues.iter().any(|s| s != "Image sample, mask and compositing coverage requires independent reconciliation")
    {
        return Ok(incomplete("Raw image/vector census contains unsupported printable context".into()));
    }
    let info = vectorcraft_pdf::production_info(bytes).map_err(|e| e.to_string())?;
    if info.intents.as_ref().is_some_and(|a| !a.is_empty()) || info.pages.iter().any(|p| p.intents.as_ref().is_some_and(|a| !a.is_empty())) {
        return Ok(incomplete("Output-intent colour conversion requires profile-specific qualification".into()));
    }
    if info.pages.len() != order.images.len() {
        return Err("Raw image page inventories differ".into());
    }
    // Refuse unsupported samples/state and unresolved resolution before decoding
    // an editable preview solely to obtain paths or clip context.
    for (page, images) in info.pages.iter().zip(&order.images) {
        if let Err(reason) = paints(page, images, uncertainty) {
            return Ok(incomplete(reason));
        }
    }
    let path_census = if order.pages.iter().any(|p| !p.is_empty()) || order.images.iter().flatten().any(|i| i.clip_context) {
        let imported = vectorcraft_pdf::import_with_report(
            bytes,
            &vectorcraft_pdf::ImportOptions {
                preserve_clip_paths: true,
                reject_depth_loss: true,
                text_as: vectorcraft_pdf::TextAs::Outlines,
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
        let mut census = crate::pdf::census_for_raw_image_prediction(&imported.document);
        // Fresh raw page metadata supplies the boxes that the editable importer
        // cannot round-trip. Validate its physical page frame before treating
        // that exact metadata-retention warning as irrelevant to this prediction.
        if census.page_sizes_mm.len() != info.pages.len()
            || census.page_sizes_mm.iter().zip(&info.pages).any(|(size, page)| {
                let width = page.crop_mm[2].min(page.media_mm[2]) - page.crop_mm[0].max(page.media_mm[0]);
                let height = page.crop_mm[3].min(page.media_mm[3]) - page.crop_mm[1].max(page.media_mm[1]);
                let expected = if matches!(page.rotation, 90 | 270) { [height, width] } else { [width, height] };
                size.iter().zip(expected).any(|(observed, expected)| !observed.is_finite() || (*observed - expected).abs() > 0.000001)
            })
        {
            return Ok(incomplete("Native and raw physical page frames differ".into()));
        }
        census.unresolved.extend(
            crate::pdf::import_messages(imported.warnings)
                .0
                .into_iter()
                .filter(|w| w != "original PDF page boxes are not retained for a rotated, scaled-unit or artwork-bounds import"),
        );
        if let Err(reason) = crate::ordering::reconcile_paths_for_images(&order, &mut census) {
            return Ok(incomplete(reason));
        }
        Some(census)
    } else {
        None
    };
    let geometry_error = path_census
        .as_ref()
        .map(|c| {
            c.paints
                .iter()
                .map(|p| p.geometry_error_mm)
                .fold(if c.prediction_images.iter().any(|i| i.clip_mm.is_some()) { 0.00001 } else { 0.0 }, f64::max)
        })
        .unwrap_or(0.0);
    if !geometry_error.is_finite() || geometry_error < 0.0 {
        return Ok(incomplete("Mixed geometry uncertainty invalid".into()));
    }
    let uncertainty = uncertainty + geometry_error;
    let incomplete = |reason: String| Comparison { status: Status::Incomplete, uncertainty_mm: uncertainty, planes: vec![], issues: vec![reason] };
    if tolerance.is_none_or(|t| t < uncertainty) {
        return Ok(incomplete("Independent resolution cannot resolve mixed geometry uncertainty".into()));
    }
    let mut image_contexts: std::collections::BTreeMap<Option<usize>, std::collections::VecDeque<&crate::pdf::PredictionImage>> = Default::default();
    if let Some(census) = &path_census {
        for image in &census.prediction_images {
            let layer = order.layers.iter().position(|name| name == &image.layer);
            image_contexts.entry(layer).or_default().push_back(image);
        }
    }
    let mut page_paints = vec![];
    let mut retained_segments = 0usize;
    for (page_number, (page, images)) in info.pages.iter().zip(&order.images).enumerate() {
        let mut image_paints = match paints(page, images, uncertainty) {
            Ok(p) => p,
            Err(e) => return Ok(incomplete(e)),
        };
        for (raw, paint) in images.iter().zip(&mut image_paints) {
            if let Some(census) = &path_census {
                let Some(context) = image_contexts.get_mut(&raw.layer).and_then(|q| q.pop_front()) else {
                    return Ok(incomplete("Native image context is missing".into()));
                };
                let Some(origin) = census.page_origins_mm.get(page_number) else {
                    return Ok(incomplete("Native image page origin missing".into()));
                };
                if let Err(reason) = reconcile_image_clip(raw, context, origin, paint) {
                    return Ok(incomplete(reason));
                }
                if let Some(clip) = &paint.clip
                    && let Err(reason) = crate::raster::retain_boundary(clip, &mut retained_segments)
                {
                    return Ok(incomplete(reason));
                }
            } else if raw.clip_context {
                return Ok(incomplete("Image clipping context was not reconciled".into()));
            }
        }
        let mut image_paints = images.iter().zip(image_paints).peekable();
        let mut vectors = vec![];
        if let Some(census) = &path_census {
            for paint in census.paints.iter().filter(|p| p.page == Some(page_number)) {
                let vector = match vector_paint(paint, census) {
                    Ok(vector) => vector,
                    Err(reason) => return Ok(incomplete(reason)),
                };
                if let Err(reason) = crate::raster::retain_boundary(&vector.boundary, &mut retained_segments) {
                    return Ok(incomplete(reason));
                }
                vectors.push(vector);
            }
        }
        let path_count = order.pages.get(page_number).ok_or("Mixed path page missing")?.len();
        if vectors.len() != path_count {
            return Ok(incomplete("Mixed vector page count differs from raw path events".into()));
        }
        let mut vectors = vectors.into_iter();
        let mut events = vec![];
        for before_path in 0..=path_count {
            while image_paints.peek().is_some_and(|(image, _)| image.before_path == before_path) {
                if let Some((_, image)) = image_paints.next() {
                    events.push(PaintEvent::Image(image));
                }
            }
            if let Some(vector) = vectors.next() {
                events.push(PaintEvent::Vector(vector));
            }
        }
        if image_paints.next().is_some() {
            return Ok(incomplete("Image/path event order is inconsistent".into()));
        }
        page_paints.push(events);
    }
    if image_contexts.values().any(|images| !images.is_empty()) {
        return Ok(incomplete("Native image contexts remain unmatched".into()));
    }
    let mut inks: BTreeSet<String> = ["Cyan", "Magenta", "Yellow", "Black"].into_iter().map(String::from).collect();
    for event in page_paints.iter().flatten() {
        match event {
            PaintEvent::Image(p) => inks.extend(p.samples.colourants.iter().cloned()),
            PaintEvent::Vector(p) => inks.extend(p.inks.keys().cloned()),
        }
    }
    let expected: BTreeSet<_> = (1..=info.pages.len() as u32).flat_map(|page| inks.iter().cloned().map(move |name| (page, name))).collect();
    let actual: BTreeSet<_> = render.planes.iter().map(|p| (p.page, p.name.clone())).collect();
    if expected != actual || actual.len() != render.planes.len() {
        return Ok(incomplete("Independent page/colourant inventory differs from raw image census".into()));
    }
    let mut planes = vec![];
    let mut unresolved_blending = false;
    let mut pixels = 0u64;
    let mut evaluations = 0u64;
    let mut boundary_evaluations = 0usize;
    for plane in &render.planes {
        let plane_pixels = u64::from(plane.width) * u64::from(plane.height);
        if plane_pixels == 0 || plane_pixels > 20_000_000 - pixels {
            return Ok(crate::raster::interrupted(planes, None, uncertainty, "Independent image comparison pixel budget exceeded".into()));
        }
        let count = page_paints.get(plane.page as usize - 1).ok_or("Image paints page missing")?.len() as u64;
        evaluations = evaluations
            .checked_add(plane_pixels.checked_mul(count).ok_or("Image evaluation budget overflow")?)
            .ok_or("Image evaluation budget overflow")?;
        if evaluations > 80_000_000 {
            return Ok(crate::raster::interrupted(planes, None, uncertainty, "Independent image sample evaluation budget exceeded".into()));
        }
        let image = match crate::raster::read_plane(plane) {
            Ok(image) => image,
            Err(reason) => {
                return Ok(crate::raster::interrupted(planes, None, uncertainty, reason));
            }
        };
        let page = info.pages.get(plane.page as usize - 1).ok_or("Image plane page missing")?;
        let w = page.crop_mm[2].min(page.media_mm[2]) - page.crop_mm[0].max(page.media_mm[0]);
        let h = page.crop_mm[3].min(page.media_mm[3]) - page.crop_mm[1].max(page.media_mm[1]);
        let (w, h) = if page.rotation == 90 || page.rotation == 270 { (h, w) } else { (w, h) };
        if (w / render.pixel_pitch_mm - f64::from(plane.width)).abs() > 1.0 || (h / render.pixel_pitch_mm - f64::from(plane.height)).abs() > 1.0 {
            return Ok(crate::raster::interrupted(planes, None, uncertainty, "Independent image page dimensions disagree".into()));
        }
        let paints = page_paints.get(plane.page as usize - 1).ok_or("Image paints page missing")?;
        let mut result = PlaneComparison {
            page: plane.page,
            name: plane.name.clone(),
            status: Status::Pass,
            compared_pixels: 0,
            boundary_pixels: 0,
            wrong_pixels: 0,
            maximum_tone_error: 0.0,
            first_defect_mm: None,
        };
        for (x, y, pixel) in image.enumerate_pixels() {
            pixels += 1;
            if pixels > 20_000_000 {
                return Ok(crate::raster::interrupted(
                    planes,
                    Some(result),
                    uncertainty,
                    "Independent image comparison pixel budget exceeded".into(),
                ));
            }
            let point = Point::new((f64::from(x) + 0.5) * render.pixel_pitch_mm, (f64::from(y) + 0.5) * render.pixel_pitch_mm);
            let mut expected = 0.0;
            let vector_tone_error = 1.0 / 255.0 + f64::from(f32::EPSILON);
            let mut allowed_tone_error = vector_tone_error;
            let mut boundary = false;
            let mut fractional_layers = 0;
            for paint in paints {
                let sample = match paint.sample(point, &plane.name, uncertainty, &mut boundary_evaluations) {
                    Ok(sample) => sample,
                    Err(reason) => {
                        return Ok(crate::raster::interrupted(planes, Some(result), uncertainty, reason));
                    }
                };
                match sample {
                    Ok(Some((tone, alpha))) => {
                        expected = expected * (1.0 - alpha) + tone * alpha;
                        // Retain the vector-only quantisation envelope wherever an
                        // opaque vector is the last writer. Fractional image writes
                        // keep the existing source/mask envelope for visible mixtures.
                        let own_error = match paint {
                            PaintEvent::Image(_) => 2.0 / 255.0,
                            PaintEvent::Vector(_) => vector_tone_error,
                        };
                        if alpha == 1.0 {
                            allowed_tone_error = own_error;
                        } else if alpha > 0.0 {
                            allowed_tone_error = allowed_tone_error.max(own_error);
                        }
                        if alpha == 1.0 {
                            fractional_layers = 0;
                        } else if alpha > 0.0 {
                            fractional_layers += 1;
                        }
                    }
                    Ok(None) => {}
                    Err(()) => boundary = true,
                }
            }
            if fractional_layers > 1 {
                unresolved_blending = true;
                result.boundary_pixels += 1;
                continue;
            }
            if boundary {
                result.boundary_pixels += 1;
                continue;
            }
            result.compared_pixels += 1;
            let error = (1.0 - f64::from(pixel.0[0]) / 255.0 - expected).abs();
            result.maximum_tone_error = result.maximum_tone_error.max(error);
            if error > allowed_tone_error {
                result.status = Status::Fail;
                result.wrong_pixels += 1;
                if result.first_defect_mm.is_none() {
                    result.first_defect_mm = Some([point.x, point.y]);
                }
            }
        }
        if result.compared_pixels == 0 {
            result.status = Status::Incomplete;
        }
        planes.push(result);
    }
    let status = if planes.iter().any(|p| p.status == Status::Fail) {
        Status::Fail
    } else if unresolved_blending || planes.iter().any(|p| p.status == Status::Incomplete) {
        Status::Incomplete
    } else {
        Status::Pass
    };
    Ok(Comparison {
        status,
        uncertainty_mm: uncertainty,
        planes,
        issues: if unresolved_blending { vec!["Overlapping fractional-alpha precision requires stack qualification".into()] } else { vec![] },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pdf(rotate: u16, unit: f64, mask: bool, state: &str) -> Vec<u8> {
        let content = format!("{state} q 72 0 0 72 0 0 cm /Im Do Q");
        let mask_entry = if mask { "/SMask 6 0 R" } else { "" };
        let objects=[
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Rotate {rotate} /UserUnit {unit} /Resources << /XObject << /Im 5 0 R >> /ExtGState << /GS << /ca 0.5 >> >> >> /Contents 4 0 R >>"),
            format!("<< /Length {} >>\nstream\n{content}\nendstream",content.len()),
            format!("<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceCMYK {mask_entry} /Filter /ASCIIHexDecode /Length 33 >>\nstream\nff00000000ff00000000ff00000000ff>\nendstream"),
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode /Length 9 >>\nstream\n00ff80ff>\nendstream".into(),
        ];
        objects_pdf(&objects)
    }
    fn objects_pdf(objects: &[String]) -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![0];
        for (i, o) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = bytes.len();
        bytes.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).bytes());
        for n in offsets.iter().skip(1) {
            bytes.extend(format!("{n:010} 00000 n \n").bytes());
        }
        bytes.extend(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len()).bytes());
        bytes
    }
    #[test]
    fn image_prediction_tracks_physical_rotate_units_and_graphics_state() {
        for (rotate, centre) in
            [(0, Point::new(12.7, 12.7)), (90, Point::new(38.1, 12.7)), (180, Point::new(38.1, 38.1)), (270, Point::new(12.7, 38.1))]
        {
            let bytes = pdf(rotate, 2.0, true, "");
            let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
            let info = vectorcraft_pdf::production_info(&bytes).unwrap();
            let paints = paints(&info.pages[0], &raw.images[0], 0.001).unwrap();
            assert_eq!(paints[0].sample(centre, "Cyan").unwrap(), Some((1.0, 0.0)));
        }
        let bytes = pdf(0, 1.0, false, "/GS gs");
        let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
        let info = vectorcraft_pdf::production_info(&bytes).unwrap();
        assert!(paints(&info.pages[0], &raw.images[0], 0.001).is_err());
    }
    fn evidence_directory(name: &str, dpi: u32) -> std::path::PathBuf {
        let root = std::env::var_os("CHECK_IMAGE_EVIDENCE_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        root.join(format!("vc-image-{name}-{dpi}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))
    }
    fn retain_result(output: &std::path::Path, render: &Render, result: &Comparison, name: &str) {
        if std::env::var_os("CHECK_IMAGE_EVIDENCE_DIR").is_some() {
            std::fs::write(
                output.join(format!("{name}.json")),
                serde_json::to_vec(&serde_json::json!({"engine_version":crate::ENGINE_VERSION,"render":render,"comparison":result})).unwrap(),
            )
            .unwrap();
        }
    }
    fn finish_evidence(output: &std::path::Path) {
        if std::env::var_os("CHECK_IMAGE_EVIDENCE_DIR").is_none() {
            std::fs::remove_dir_all(output).unwrap();
        }
    }
    #[test]
    fn independent_source_sample_matrix_agrees_without_preview_conversion() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        let function = "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 1] /N 1 >>";
        for (name, description, data, dependency) in [
            ("inline-gray-raw", "/BitsPerComponent 8 /ColorSpace /DeviceGray", "415a7a20", function),
            ("inline-gray-hex", "/BitsPerComponent 8 /ColorSpace /DeviceGray", "415a7a20", function),
            ("gray-1", "/BitsPerComponent 1 /ColorSpace /DeviceGray", "4080", function),
            ("gray-2", "/BitsPerComponent 2 /ColorSpace /DeviceGray", "10b0", function),
            ("gray-4", "/BitsPerComponent 4 /ColorSpace /DeviceGray", "05af", function),
            ("gray-8-reversed", "/BitsPerComponent 8 /ColorSpace /DeviceGray /Decode [1 0]", "0055aaff", function),
            (
                "indexed-fractional-decode",
                "/BitsPerComponent 4 /ColorSpace [/Indexed /DeviceCMYK 3 <ff00000000ff00000000ff00000000ff>] /Decode [0 3]",
                "2378",
                function,
            ),
            ("gray-16-color-key", "/BitsPerComponent 16 /ColorSpace /DeviceGray /Mask [0 0]", "0000000100ffffff", function),
            ("gray-16", "/BitsPerComponent 16 /ColorSpace /DeviceGray", "00005555aaaaffff", function),
            ("indexed-cmyk", "/BitsPerComponent 1 /ColorSpace [/Indexed /DeviceCMYK 1 <ff00000000ff0000>] /Decode [1 0]", "4080", function),
            ("spot", "/BitsPerComponent 8 /ColorSpace [/Separation /SyntheticSpot /DeviceCMYK 6 0 R]", "0055aaff", function),
            (
                "named-cyan",
                "/BitsPerComponent 8 /ColorSpace [/Separation /Cyan /DeviceCMYK 6 0 R]",
                "0055aaff",
                "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 0 0 0] /N 1 >>",
            ),
            (
                "named-cmyk",
                "/BitsPerComponent 8 /ColorSpace [/DeviceN [/Cyan /Magenta] /DeviceCMYK 6 0 R]",
                "ff0000ff5580ff00",
                "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length 7 >>\nstream\n{ 0 0 }\nendstream",
            ),
            ("color-key", "/BitsPerComponent 8 /ColorSpace /DeviceCMYK /Mask [255 255 0 0 0 0 0 0]", "ff00000000ff00000000ff00000000ff", function),
            (
                "explicit-mask",
                "/BitsPerComponent 8 /ColorSpace /DeviceCMYK /Mask 6 0 R",
                "ff00000000ff00000000ff00000000ff",
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ImageMask true /BitsPerComponent 1 /Filter /ASCIIHexDecode /Length 5 >>\nstream\n4080>\nendstream",
            ),
        ] {
            let content = match name {
                "inline-gray-raw" => "q 72 0 0 72 0 0 cm BI /W 2 /H 2 /BPC 8 /CS /G ID AZz  EI Q",
                "inline-gray-hex" => "q 72 0 0 72 0 0 cm BI /W 2 /H 2 /BPC 8 /CS /G /F /AHx ID 415a7a20> EI Q",
                _ => "q 72 0 0 72 0 0 cm /Im Do Q",
            };
            let objects = [
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>".into(),
                format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
                format!(
                    "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 {description} /Filter /ASCIIHexDecode /Length {} >>\nstream\n{data}>\nendstream",
                    data.len() + 1
                ),
                dependency.into(),
            ];
            let bytes = objects_pdf(&objects);
            for dpi in [72, 144] {
                let output = evidence_directory(name, dpi);
                let config = crate::independent::Configuration {
                    binary: binary.clone().into(),
                    binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                    version: "10.07.1".into(),
                    dpi,
                    timeout_ms: 10000,
                    max_output_bytes: 16 * 1024 * 1024,
                    qualification_sha256: None,
                };
                let render = crate::independent::render(&config, &bytes, &output).unwrap();
                let result = compare(&bytes, &render, Some(2.0)).unwrap();
                retain_result(&output, &render, &result, "comparison");
                assert_eq!(
                    result.status,
                    if matches!(name, "indexed-fractional-decode" | "gray-16-color-key") { Status::Incomplete } else { Status::Pass },
                    "{name}/{dpi}: {result:?}; diagnostics={:?}",
                    render.diagnostics
                );
                finish_evidence(&output);
            }
        }
    }
    #[test]
    fn independent_image_stacks_preserve_knockout_and_refuse_unqualified_alpha_accumulation() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        for (name, second, mask, status) in [
            ("gray-knockout", "/ColorSpace /DeviceGray", false, Status::Pass),
            ("fractional-stack", "/ColorSpace /DeviceCMYK /SMask 7 0 R", true, Status::Incomplete),
            ("opaque-over-fractional", "/ColorSpace /DeviceGray", true, Status::Pass),
        ] {
            let content = "q 72 0 0 72 0 0 cm /A Do /B Do Q";
            let first_mask = if mask { "/SMask 7 0 R" } else { "" };
            let second_data = if second.contains("DeviceGray") { "0055aaff" } else { "00ff000000ff000000ff000000ff0000" };
            let objects=[
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Resources << /XObject << /A 5 0 R /B 6 0 R >> >> /Contents 4 0 R >>".into(),
                format!("<< /Length {} >>\nstream\n{content}\nendstream",content.len()),
                format!("<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceCMYK {first_mask} /Filter /ASCIIHexDecode /Length 33 >>\nstream\nff000000ff000000ff000000ff000000>\nendstream"),
                format!("<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 {second} /Filter /ASCIIHexDecode /Length {} >>\nstream\n{second_data}>\nendstream",second_data.len()+1),
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode /Length 9 >>\nstream\n80808080>\nendstream".into(),
            ];
            let bytes = objects_pdf(&objects);
            for dpi in [72, 144] {
                let output = evidence_directory(name, dpi);
                let config = crate::independent::Configuration {
                    binary: binary.clone().into(),
                    binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                    version: "10.07.1".into(),
                    dpi,
                    timeout_ms: 10000,
                    max_output_bytes: 16 * 1024 * 1024,
                    qualification_sha256: None,
                };
                let render = crate::independent::render(&config, &bytes, &output).unwrap();
                let result = compare(&bytes, &render, Some(2.0)).unwrap();
                retain_result(&output, &render, &result, "comparison");
                assert_eq!(result.status, status, "{name}/{dpi}: {result:?}");
                finish_evidence(&output);
            }
        }
    }
    #[test]
    fn actual_interpreter_agrees_on_image_tones_masks_rotation_and_units_and_detects_defects() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        for (rotate, unit, mask) in [(0, 1.0, false), (0, 1.0, true), (90, 1.0, true), (180, 2.0, true), (270, 1.0, true)] {
            let bytes = pdf(rotate, unit, mask, "");
            for dpi in [72, 144] {
                let output = evidence_directory(&format!("cmyk-{rotate}-{unit}-{mask}"), dpi);
                let config = crate::independent::Configuration {
                    binary: binary.clone().into(),
                    binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                    version: "10.07.1".into(),
                    dpi,
                    timeout_ms: 10000,
                    max_output_bytes: 16 * 1024 * 1024,
                    qualification_sha256: None,
                };
                let mut render = crate::independent::render(&config, &bytes, &output).unwrap();
                let result = compare(&bytes, &render, Some(2.0)).unwrap();
                retain_result(&output, &render, &result, "comparison");
                assert_eq!(result.status, Status::Pass, "{rotate}/{unit}/{mask}/{dpi}: {result:?}");
                let plane = render.planes.iter_mut().find(|p| p.name == "Cyan").unwrap();
                let mut image = image::open(&plane.path).unwrap().into_luma8();
                let p = image.get_pixel_mut(plane.width / 4, plane.height / 4);
                p.0[0] = 255 - p.0[0];
                plane.path = output.join("mutated-cyan.tif");
                image.save(&plane.path).unwrap();
                plane.sha256 = crate::sha256(&std::fs::read(&plane.path).unwrap());
                let defect = compare(&bytes, &render, Some(2.0)).unwrap();
                retain_result(&output, &render, &defect, "defect");
                assert_eq!(defect.status, Status::Fail);
                finish_evidence(&output);
            }
        }
    }
    fn mixed_pdf(stream: &str, rotate: u16, unit: f64, mask: bool, crop: bool) -> Vec<u8> {
        let mask_entry = if mask { "/SMask 6 0 R" } else { "" };
        let crop_entry = if crop { "/CropBox [6 4 90 68]" } else { "" };
        objects_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [8 0 R 9 0 R] /D << /Order [9 0 R 8 0 R] /ON [8 0 R 9 0 R] >> >> >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 96 72] {crop_entry} /Rotate {rotate} /UserUnit {unit} /Resources << /XObject << /Im 5 0 R /MaskedForm 11 0 R /PlainForm 13 0 R >> /Properties << /Lower 8 0 R /Upper 9 0 R >> /ColorSpace << /Ink [/Separation /SyntheticSpot /DeviceCMYK 7 0 R] >> /ExtGState << /OP0 << /op true /OPM 0 >> /OP1 << /op true /OPM 1 >> /Alpha << /ca 0.5 >> /Multiply << /BM /Multiply >> /Soft << /SMask << /S /Alpha /G 10 0 R >> >> /Transfer << /TR 12 0 R >> /Defaults << /TR2 /Default /BG2 /Default /UCR2 /Default /HT /Default >> /Identity << /TR /Identity >> >> >> /Contents 4 0 R >>"),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream",stream.len()),
            format!("<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceCMYK {mask_entry} /Filter /ASCIIHexDecode /Length 33 >>\nstream\nff00000000ff00000000ff00000000ff>\nendstream"),
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode /Length 9 >>\nstream\n00ff80ff>\nendstream".into(),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 1] /N 1 >>".into(),
            "<< /Type /OCG /Name (Lower) >>".into(),
            "<< /Type /OCG /Name (Upper) >>".into(),
            {let content="1 g 0 0 96 72 re f";format!("<< /Type /XObject /Subtype /Form /BBox [0 0 96 72] /Group << /S /Transparency /CS /DeviceGray >> /Resources << >> /Length {} >>\nstream\n{content}\nendstream",content.len())},
            {let content="/Soft gs q 48 0 0 48 12 12 cm /Im Do Q";format!("<< /Type /XObject /Subtype /Form /BBox [0 0 96 72] /Group << /S /Transparency /CS /DeviceCMYK >> /Resources << /XObject << /Im 5 0 R >> /ExtGState << /Soft << /SMask << /S /Alpha /G 10 0 R >> >> >> >> /Length {} >>\nstream\n{content}\nendstream",content.len())},
            "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>".into(),
            {let content=IMAGE;format!("<< /Type /XObject /Subtype /Form /BBox [0 0 96 72] /Resources << /XObject << /Im 5 0 R >> >> /Length {} >>\nstream\n{content}\nendstream",content.len())},
        ])
    }
    const IMAGE: &str = "q 48 0 0 48 12 12 cm /Im Do Q";
    const BACKGROUND: &str = "0.4 0.2 0 0 k 8 6 80 60 re f";
    const FOREGROUND: &str = "0 0.7 0.3 0 k 24 18 48 36 re f";
    #[test]
    fn mixed_path_reconciliation_keeps_full_census_gaps_and_detects_missing_images() {
        let bytes = mixed_pdf(&format!("{BACKGROUND} {IMAGE} {FOREGROUND}"), 0, 1.0, false, false);
        let doc = vectorcraft_pdf::import(&bytes).unwrap();
        let ordinary = crate::pdf::census(&doc);
        assert!(!ordinary.unresolved.is_empty());
        let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
        assert_eq!(raw.images[0][0].before_path, 1);
        let mut paths = crate::pdf::census_for_raw_image_prediction(&doc);
        crate::ordering::reconcile_paths_for_images(&raw, &mut paths).unwrap();
        assert_eq!(paths.paints.len(), 2);
        assert!(paths.paint_order_verified);
        paths.image_objects = 0;
        assert!(crate::ordering::reconcile_paths_for_images(&raw, &mut paths).is_err());
        let mut unsupported = raw.clone();
        unsupported.issues.insert("Unrelated printable context".into());
        paths.image_objects = 1;
        assert!(crate::ordering::reconcile_paths_for_images(&unsupported, &mut paths).is_err());
    }
    #[test]
    fn actual_interpreter_mixed_images_vectors_preserve_paint_order_and_bound_unsupported_context() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        let cases = [
            ("vector-image", format!("{BACKGROUND} {IMAGE}"), 0, 1.0, false, false, Status::Pass),
            ("image-vector", format!("{IMAGE} {FOREGROUND}"), 0, 1.0, false, false, Status::Pass),
            ("vector-mask-vector", format!("{BACKGROUND} {IMAGE} {FOREGROUND}"), 0, 1.0, true, false, Status::Pass),
            ("spot-mask", format!("/Ink cs 0.6 scn 8 6 80 60 re f {IMAGE}"), 0, 1.0, true, false, Status::Pass),
            ("mask-spot", format!("{IMAGE} /Ink cs 0.6 scn 24 18 48 36 re f"), 0, 1.0, true, false, Status::Pass),
            (
                "layer-order",
                format!("/OC /Lower BDC {BACKGROUND} EMC {IMAGE} /OC /Upper BDC {FOREGROUND} EMC /OC /Lower BDC 0 0 0 0.5 k 30 24 24 24 re f EMC"),
                0,
                1.0,
                false,
                false,
                Status::Pass,
            ),
            ("vector-opm0", format!("{IMAGE} /OP0 gs {FOREGROUND}"), 0, 1.0, false, false, Status::Pass),
            ("vector-opm1", format!("{IMAGE} /OP1 gs {FOREGROUND}"), 0, 1.0, false, false, Status::Pass),
            ("gray-overprint", format!("{IMAGE} /OP1 gs 0.4 g 24 18 48 36 re f"), 0, 1.0, false, false, Status::Pass),
            ("rotate90", format!("{BACKGROUND} {IMAGE} {FOREGROUND}"), 90, 1.0, true, false, Status::Pass),
            ("rotate180-unit2", format!("{BACKGROUND} {IMAGE} {FOREGROUND}"), 180, 2.0, true, false, Status::Pass),
            ("rotate270-crop", format!("{BACKGROUND} {IMAGE} {FOREGROUND}"), 270, 1.0, true, true, Status::Pass),
            ("vector-clip", format!("{IMAGE} q 18 18 48 36 re 30 24 24 24 re W* n {FOREGROUND} Q"), 0, 1.0, false, false, Status::Pass),
            ("image-alpha", format!("{BACKGROUND} /Alpha gs {IMAGE}"), 0, 1.0, false, false, Status::Incomplete),
            ("image-gs-softmask", format!("{BACKGROUND} /Soft gs {IMAGE}"), 0, 1.0, false, false, Status::Incomplete),
            ("form-gs-softmask", format!("{BACKGROUND} /MaskedForm Do"), 0, 1.0, false, false, Status::Incomplete),
            ("vector-transfer", format!("{IMAGE} /Transfer gs {FOREGROUND}"), 0, 1.0, false, false, Status::Incomplete),
            ("transfer-identity", format!("/Identity gs {BACKGROUND} {IMAGE} {FOREGROUND}"), 0, 1.0, false, false, Status::Pass),
            ("transfer-default", format!("/Defaults gs {BACKGROUND} {IMAGE} {FOREGROUND}"), 0, 1.0, false, false, Status::Pass),
            ("transfer-restored", format!("q /Transfer gs Q {IMAGE} {FOREGROUND}"), 0, 1.0, false, false, Status::Pass),
            ("image-clip", format!("{BACKGROUND} q 20 20 20 20 re W n {IMAGE} Q"), 0, 1.0, false, false, Status::Pass),
            ("vector-alpha", format!("{IMAGE} /Alpha gs {FOREGROUND}"), 0, 1.0, false, false, Status::Incomplete),
            ("vector-blend", format!("{IMAGE} /Multiply gs {FOREGROUND}"), 0, 1.0, false, false, Status::Incomplete),
        ];
        let mut failures = vec![];
        for (name, stream, rotate, unit, mask, crop, status) in cases {
            let bytes = mixed_pdf(&stream, rotate, unit, mask, crop);
            for dpi in [72, 144] {
                let output = evidence_directory(&format!("mixed-{name}"), dpi);
                let config = crate::independent::Configuration {
                    binary: binary.clone().into(),
                    binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                    version: "10.07.1".into(),
                    dpi,
                    timeout_ms: 10000,
                    max_output_bytes: 16 * 1024 * 1024,
                    qualification_sha256: None,
                };
                let mut render = crate::independent::render(&config, &bytes, &output).unwrap();
                let result = compare(&bytes, &render, Some(2.0)).unwrap();
                retain_result(&output, &render, &result, "comparison");
                if result.status != status {
                    let imported = vectorcraft_pdf::import_with_report(
                        &bytes,
                        &vectorcraft_pdf::ImportOptions { preserve_clip_paths: true, reject_depth_loss: true, ..Default::default() },
                    )
                    .unwrap();
                    let c = crate::pdf::census_for_raw_image_prediction(&imported.document);
                    failures.push(format!(
                        "mixed-{name}/{dpi}: {result:?}; expected={status:?}; census={:?}; warnings={:?}",
                        c.unresolved, imported.warnings
                    ));
                    finish_evidence(&output);
                    continue;
                }
                assert!(!render.qualified);
                if status == Status::Pass {
                    let plane = render.planes.iter_mut().find(|p| p.name == "Cyan").unwrap();
                    let mut image = image::open(&plane.path).unwrap().into_luma8();
                    // A small interior patch includes witnessed pixels even when one
                    // sample falls in a legitimate vector/image boundary envelope.
                    for y in (plane.height / 3).saturating_sub(4)..(plane.height / 3 + 5).min(plane.height) {
                        for x in (plane.width / 3).saturating_sub(4)..(plane.width / 3 + 5).min(plane.width) {
                            let pixel = image.get_pixel_mut(x, y);
                            pixel.0[0] = if pixel.0[0] > 127 { 0 } else { 255 };
                        }
                    }
                    plane.path = output.join("mutated-cyan.tif");
                    image.save(&plane.path).unwrap();
                    plane.sha256 = crate::sha256(&std::fs::read(&plane.path).unwrap());
                    let defect = compare(&bytes, &render, Some(2.0)).unwrap();
                    retain_result(&output, &render, &defect, "defect");
                    if defect.status != Status::Fail {
                        failures.push(format!("mixed-{name}/{dpi} changed interior pixels: {defect:?}"));
                    }
                }
                finish_evidence(&output);
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
    #[test]
    fn mixed_vector_only_pixels_keep_the_vector_tone_threshold() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        let bytes = mixed_pdf(&format!("{IMAGE} {FOREGROUND}"), 0, 1.0, false, false);
        let output = evidence_directory("vector-quantisation", 72);
        let config = crate::independent::Configuration {
            binary: binary.clone().into(),
            binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
            version: "10.07.1".into(),
            dpi: 72,
            timeout_ms: 10000,
            max_output_bytes: 16 * 1024 * 1024,
            qualification_sha256: None,
        };
        let mut render = crate::independent::render(&config, &bytes, &output).unwrap();
        assert_eq!(compare(&bytes, &render, Some(2.0)).unwrap().status, Status::Pass);
        let mut unchanged = render.clone();
        let plane = render.planes.iter_mut().find(|p| p.name == "Cyan").unwrap();
        let mut image = image::open(&plane.path).unwrap().into_luma8();
        assert_eq!(image.get_pixel(40, 30).0[0], 255);
        image.get_pixel_mut(40, 30).0[0] = 253;
        plane.path = output.join("two-step-vector-defect.tif");
        image.save(&plane.path).unwrap();
        plane.sha256 = crate::sha256(&std::fs::read(&plane.path).unwrap());
        let defect = compare(&bytes, &render, Some(2.0)).unwrap();
        retain_result(&output, &render, &defect, "quantisation-defect");
        assert_eq!(defect.status, Status::Fail);
        unchanged.planes.first_mut().unwrap().sha256 = "0".repeat(64);
        let unavailable = compare(&bytes, &unchanged, Some(2.0)).unwrap();
        assert_eq!(unavailable.status, Status::Incomplete);
        render.planes.iter_mut().find(|p| p.name == "Yellow").unwrap().sha256 = "0".repeat(64);
        let retained = compare(&bytes, &render, Some(2.0)).unwrap();
        assert_eq!(retained.status, Status::Fail);
        assert!(retained.issues.iter().any(|s| s.contains("plane changed")));
        finish_evidence(&output);
    }
    #[test]
    fn independent_boundary_budget_retains_an_incomplete_report() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        let bytes = mixed_pdf(&format!("{} {IMAGE}", format!("{BACKGROUND} ").repeat(80)), 0, 1.0, false, false);
        let output = evidence_directory("mixed-boundary-budget", 144);
        let config = crate::independent::Configuration {
            binary: binary.clone().into(),
            binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
            version: "10.07.1".into(),
            dpi: 144,
            timeout_ms: 10000,
            max_output_bytes: 32 * 1024 * 1024,
            qualification_sha256: None,
        };
        let mut request = crate::tests::request();
        request.identity.pdf_sha256 = crate::sha256(&bytes);
        request.identity.recipe_sha256 = crate::digest(&request.recipe).unwrap();
        request.acceptance.insert(
            "geometry_tolerance_mm".into(),
            crate::preparation::Fact {
                value: serde_json::json!(2.0),
                provenance: crate::preparation::Provenance::Operator {
                    actor: "Synthetic budget test".into(),
                    at: "2026-10-19".into(),
                    decision_id: "synthetic".into(),
                    evidence_sha256: "e".repeat(64),
                },
            },
        );
        request.identity.measurement_sha256 = crate::digest(&request.acceptance).unwrap();
        let verification = crate::independent::verify(&request, &bytes, &config, &output).unwrap();
        assert!(
            verification
                .comparisons
                .iter()
                .any(|c| { c.status == Status::Incomplete && c.issues.iter().any(|s| s.contains("Boundary distance budget exceeded")) })
        );
        let finding = verification.report.findings.iter().find(|f| f.obligation.key == "global/independent").unwrap();
        assert_eq!(finding.status, Status::Incomplete);
        assert_eq!(verification.report.coverage.total, crate::enumerate(&request).unwrap().len());
        let mut altered = verification.renderings[0].clone();
        let plane = altered.planes.first_mut().unwrap();
        let mut image = image::open(&plane.path).unwrap().into_luma8();
        let pixel = image.get_pixel_mut(160, 60);
        pixel.0[0] = if pixel.0[0] > 127 { 0 } else { 255 };
        plane.path = output.join("early-defect-before-budget.tif");
        image.save(&plane.path).unwrap();
        plane.sha256 = crate::sha256(&std::fs::read(&plane.path).unwrap());
        let partial = compare(&bytes, &altered, Some(2.0)).unwrap();
        retain_result(&output, &altered, &partial, "budget-defect");
        assert_eq!(partial.status, Status::Fail);
        assert!(partial.issues.iter().any(|s| s.contains("Boundary distance budget exceeded")));
        assert!(partial.planes.iter().any(|p| p.wrong_pixels > 0));
        finish_evidence(&output);
    }
    #[test]
    fn actual_interpreter_image_clipping_retains_holes_repeated_uses_and_page_transforms() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        let clip = "18 18 36 36 re W n";
        let hole = "12 12 48 48 re 24 24 24 24 re W* n";
        let circle = "60 36 m 60 49.254834 49.254834 60 36 60 c 22.745166 60 12 49.254834 12 36 c 12 22.745166 22.745166 12 36 12 c 49.254834 12 60 22.745166 60 36 c h W n";
        let cases = [
            ("clip-curve", format!("q {circle} {IMAGE} Q"), 0, 1.0, false),
            ("clip-enclosing", format!("q 12 12 48 48 re W n {IMAGE} Q"), 0, 1.0, false),
            ("clip-page-wide", format!("q 0 0 96 72 re W n {IMAGE} Q"), 0, 1.0, false),
            ("clip-crop-media", format!("q 0 0 96 72 re W n {IMAGE} Q"), 0, 1.0, false),
            ("clip-crop-form", "/PlainForm Do".into(), 0, 1.0, false),
            ("clip-form", "/PlainForm Do".into(), 0, 1.0, false),
            ("clip-only", format!("q {clip} {IMAGE} Q"), 0, 1.0, false),
            ("clip-hole", format!("q {hole} {IMAGE} Q"), 0, 1.0, false),
            ("clip-mask", format!("{BACKGROUND} q {hole} {IMAGE} Q {FOREGROUND}"), 0, 1.0, true),
            ("clip-nested", format!("q {clip} q 20 20 30 30 re W n {IMAGE} Q Q"), 0, 1.0, false),
            ("clip-repeated", format!("q {clip} {IMAGE} Q q 48 24 24 24 re W n q 48 0 0 48 36 12 cm /Im Do Q Q"), 0, 1.0, false),
            (
                "clip-layer-order",
                format!("/OC /Lower BDC q {clip} {IMAGE} Q EMC /OC /Upper BDC {FOREGROUND} EMC /OC /Lower BDC q {hole} {IMAGE} Q EMC"),
                0,
                1.0,
                false,
            ),
            ("clip-rotate90", format!("q {hole} {IMAGE} Q"), 90, 1.0, true),
            ("clip-rotate180-unit2", format!("{BACKGROUND} q {clip} {IMAGE} Q"), 180, 2.0, true),
            ("clip-rotate270", format!("{BACKGROUND} q {hole} {IMAGE} Q"), 270, 1.0, true),
        ];
        let mut failures = vec![];
        for (name, stream, rotate, unit, mask) in cases {
            let bytes = mixed_pdf(&stream, rotate, unit, mask, name.starts_with("clip-crop-"));
            for dpi in [72, 144] {
                let output = evidence_directory(&format!("image-{name}"), dpi);
                let config = crate::independent::Configuration {
                    binary: binary.clone().into(),
                    binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                    version: "10.07.1".into(),
                    dpi,
                    timeout_ms: 10000,
                    max_output_bytes: 16 * 1024 * 1024,
                    qualification_sha256: None,
                };
                let mut render = crate::independent::render(&config, &bytes, &output).unwrap();
                let result = compare(&bytes, &render, Some(2.0)).unwrap();
                retain_result(&output, &render, &result, "comparison");
                let expected = Status::Pass;
                if result.status != expected {
                    let imported = vectorcraft_pdf::import_with_report(
                        &bytes,
                        &vectorcraft_pdf::ImportOptions { preserve_clip_paths: true, reject_depth_loss: true, ..Default::default() },
                    )
                    .unwrap();
                    let census = crate::pdf::census_for_raw_image_prediction(&imported.document);
                    failures.push(format!(
                        "{name}/{dpi}: {result:?}; contexts={:?}; warnings={:?}; unresolved={:?}",
                        census.prediction_images, imported.warnings, census.unresolved
                    ));
                } else if expected == Status::Pass {
                    let plane = render.planes.iter_mut().find(|p| p.name == "Cyan").unwrap();
                    let mut image = image::open(&plane.path).unwrap().into_luma8();
                    for y in (plane.height / 2).saturating_sub(4)..(plane.height / 2 + 5).min(plane.height) {
                        for x in (plane.width / 2).saturating_sub(4)..(plane.width / 2 + 5).min(plane.width) {
                            let pixel = image.get_pixel_mut(x, y);
                            pixel.0[0] = if pixel.0[0] > 127 { 0 } else { 255 };
                        }
                    }
                    plane.path = output.join("mutated-cyan.tif");
                    image.save(&plane.path).unwrap();
                    plane.sha256 = crate::sha256(&std::fs::read(&plane.path).unwrap());
                    let defect = compare(&bytes, &render, Some(2.0)).unwrap();
                    retain_result(&output, &render, &defect, "defect");
                    if defect.status != Status::Fail {
                        failures.push(format!("{name}/{dpi} changed interior: {defect:?}"));
                    }
                }
                finish_evidence(&output);
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
    #[test]
    fn image_context_reconciliation_rejects_changed_transform_dimensions_and_missing_clip() {
        let bytes = mixed_pdf(&format!("q 18 18 36 36 re W n {IMAGE} Q"), 0, 1.0, false, false);
        let doc = vectorcraft_pdf::import(&bytes).unwrap();
        let census = crate::pdf::census_for_raw_image_prediction(&doc);
        let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
        let info = vectorcraft_pdf::production_info(&bytes).unwrap();
        let mut paints = paints(&info.pages[0], &raw.images[0], 0.0001).unwrap();
        let image = &raw.images[0][0];
        let context = &census.prediction_images[0];
        let origin = &census.page_origins_mm[0];
        assert!(reconcile_image_clip(image, context, origin, &mut paints[0]).is_ok());
        let mut changed = context.clone();
        changed.dimensions[0] += 1;
        assert!(reconcile_image_clip(image, &changed, origin, &mut paints[0]).is_err());
        changed = context.clone();
        changed.transform_mm = Affine::translate((0.1, 0.0)) * changed.transform_mm;
        assert!(reconcile_image_clip(image, &changed, origin, &mut paints[0]).is_err());
        changed = context.clone();
        changed.clip_mm = None;
        assert!(reconcile_image_clip(image, &changed, origin, &mut paints[0]).is_err());
    }
    #[test]
    fn raw_image_device_defaults_and_identity_keep_the_state_explicit() {
        for gs in ["Defaults", "Identity"] {
            let bytes = mixed_pdf(&format!("/{gs} gs {IMAGE}"), 0, 1.0, false, false);
            let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
            assert!(!raw.images[0][0].compositing_unresolved, "{gs}");
            let bytes = mixed_pdf(&format!("/Transfer gs /{gs} gs {IMAGE}"), 0, 1.0, false, false);
            let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
            assert!(raw.images[0][0].compositing_unresolved, "{gs}");
        }
    }
    #[test]
    fn duplicate_named_image_layers_stay_incomplete_before_plane_measurement() {
        let original = mixed_pdf(&format!("/OC /Lower BDC {IMAGE} EMC /OC /Upper BDC {IMAGE} EMC"), 0, 1.0, false, false);
        let text = String::from_utf8(original.clone()).unwrap().replace("/Name (Upper)", "/Name (Lower)");
        assert_eq!(text.len(), original.len()); // Equal-width edit preserves synthetic xref offsets.
        let bytes = text.into_bytes();
        let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
        assert!(raw.issues.iter().any(|s| s.contains("Duplicate PDF layer names")));
        let render = Render {
            pdf_sha256: crate::sha256(&bytes),
            configuration_sha256: "0".repeat(64),
            version: "synthetic preflight only".into(),
            planes: vec![],
            spot_names: vec![],
            diagnostics: vec![],
            pixel_pitch_mm: 0.1,
            qualified: false,
        };
        let comparison = compare(&bytes, &render, Some(2.0)).unwrap();
        assert_eq!(comparison.status, Status::Incomplete);
        assert!(comparison.issues.iter().any(|s| s.contains("unsupported printable context")));
    }
    #[test]
    fn repeated_curved_image_clips_share_an_aggregate_storage_budget() {
        let circle = "60 36 m 60 49.254834 49.254834 60 36 60 c 22.745166 60 12 49.254834 12 36 c 12 22.745166 22.745166 12 36 12 c 49.254834 12 60 22.745166 60 36 c h W n";
        let one = mixed_pdf(&format!("q {circle} {IMAGE} Q"), 0, 1.0, false, false);
        let native = vectorcraft_pdf::import(&one).unwrap();
        let census = crate::pdf::census_for_raw_image_prediction(&native);
        let clip = census.prediction_images[0].clip_mm.as_ref().unwrap();
        let boundary = crate::geometry::Boundary::new(clip, vectorcraft_geom::FillRule::NonZero, 0.000001).unwrap();
        let repetitions = 1_000_000 / boundary.retained_segments() + 1;
        assert!(repetitions < 4096);
        let bytes = mixed_pdf(&format!("q {circle} {} Q", format!("{IMAGE} ").repeat(repetitions)), 0, 1.0, false, false);
        let render = Render {
            pdf_sha256: crate::sha256(&bytes),
            configuration_sha256: "0".repeat(64),
            version: "synthetic resource preflight only".into(),
            planes: vec![],
            spot_names: vec![],
            diagnostics: vec![],
            pixel_pitch_mm: 0.1,
            qualified: false,
        };
        let result = compare(&bytes, &render, Some(2.0)).unwrap();
        assert_eq!(result.status, Status::Incomplete);
        assert!(result.issues.iter().any(|s| s.contains("aggregate geometry storage budget")), "{result:?}");
    }
}
