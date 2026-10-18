//! Raw image-only PDF prediction, independently reconciled to tiffsep planes.
//! Reject unsupported contexts before comparing. This is a feature-specific measurement,
//! not runtime qualification, a prepared hard mask, or proof of artwork fidelity.
use crate::{
    Status,
    independent::Render,
    raster::{Comparison, PlaneComparison},
};
use std::collections::BTreeSet;
use vectorcraft_geom::{Affine, Point};

struct Paint<'a> {
    samples: &'a vectorcraft_pdf::PrintImageSamples,
    inverse: Affine,
    margin: [f64; 2],
}
fn paints<'a>(
    page: &vectorcraft_pdf::ProductionPage,
    images: &'a [vectorcraft_pdf::PrintImageUse],
    uncertainty: f64,
) -> Result<Vec<Paint<'a>>, String> {
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
            if image.clip_context
                || image.overprint
                || image.compositing_unresolved
                || image.description.optional_content
                || image.description.interpolate
            {
                return Err(
                    "Image clipping, overprint, graphics state, optional content or interpolation requires broader compositing qualification".into(),
                );
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
            Ok(Paint { samples, inverse, margin })
        })
        .collect()
}
impl Paint<'_> {
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
    if order.pages.iter().any(|p| !p.is_empty())
        || order.images.iter().all(Vec::is_empty)
        || order.issues.iter().any(|s| s != "Image sample, mask and compositing coverage requires independent reconciliation")
    {
        return Ok(incomplete("Raw image-only census contains unsupported printable context".into()));
    }
    let info = vectorcraft_pdf::production_info(bytes).map_err(|e| e.to_string())?;
    if info.intents.as_ref().is_some_and(|a| !a.is_empty()) || info.pages.iter().any(|p| p.intents.as_ref().is_some_and(|a| !a.is_empty())) {
        return Ok(incomplete("Output-intent colour conversion requires profile-specific qualification".into()));
    }
    if info.pages.len() != order.images.len() {
        return Err("Raw image page inventories differ".into());
    }
    let mut page_paints = vec![];
    for (page, images) in info.pages.iter().zip(&order.images) {
        match paints(page, images, uncertainty) {
            Ok(p) => page_paints.push(p),
            Err(e) => return Ok(incomplete(e)),
        }
    }
    let mut inks: BTreeSet<String> = ["Cyan", "Magenta", "Yellow", "Black"].into_iter().map(String::from).collect();
    for p in page_paints.iter().flatten() {
        inks.extend(p.samples.colourants.iter().cloned());
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
    for plane in &render.planes {
        let plane_pixels = u64::from(plane.width) * u64::from(plane.height);
        if plane_pixels == 0 || plane_pixels > 20_000_000 - pixels {
            return Err("Independent image comparison pixel budget exceeded".into());
        }
        let count = page_paints.get(plane.page as usize - 1).ok_or("Image paints page missing")?.len() as u64;
        evaluations = evaluations
            .checked_add(plane_pixels.checked_mul(count).ok_or("Image evaluation budget overflow")?)
            .ok_or("Image evaluation budget overflow")?;
        if evaluations > 80_000_000 {
            return Err("Independent image sample evaluation budget exceeded".into());
        }
        let raw = std::fs::read(&plane.path).map_err(|e| e.to_string())?;
        if crate::sha256(&raw) != plane.sha256 {
            return Err("Independent image plane changed".into());
        }
        let dimensions = image::ImageReader::new(std::io::Cursor::new(&raw))
            .with_guessed_format()
            .map_err(|e| e.to_string())?
            .into_dimensions()
            .map_err(|e| e.to_string())?;
        if dimensions != (plane.width, plane.height) {
            return Err("Independent image plane dimensions changed".into());
        }
        let image = image::load_from_memory(&raw).map_err(|e| e.to_string())?.into_luma8();
        if image.dimensions() != (plane.width, plane.height) {
            return Err("Independent image plane dimensions changed".into());
        }
        let page = info.pages.get(plane.page as usize - 1).ok_or("Image plane page missing")?;
        let w = page.crop_mm[2].min(page.media_mm[2]) - page.crop_mm[0].max(page.media_mm[0]);
        let h = page.crop_mm[3].min(page.media_mm[3]) - page.crop_mm[1].max(page.media_mm[1]);
        let (w, h) = if page.rotation == 90 || page.rotation == 270 { (h, w) } else { (w, h) };
        if (w / render.pixel_pitch_mm - f64::from(plane.width)).abs() > 1.0 || (h / render.pixel_pitch_mm - f64::from(plane.height)).abs() > 1.0 {
            return Ok(incomplete("Independent image page dimensions disagree".into()));
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
                return Err("Independent image comparison pixel budget exceeded".into());
            }
            let point = Point::new((f64::from(x) + 0.5) * render.pixel_pitch_mm, (f64::from(y) + 0.5) * render.pixel_pitch_mm);
            let mut expected = 0.0;
            let mut boundary = false;
            let mut fractional_layers = 0;
            for paint in paints {
                match paint.sample(point, &plane.name) {
                    Ok(Some((tone, alpha))) => {
                        expected = expected * (1.0 - alpha) + tone * alpha;
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
            if error > 2.0 / 255.0 {
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
}
