//! Fill replacement edits selected image instances, never their shared source blob.
use super::{Result, error, profiles};
use vectorcraft_color::Paint;
use vectorcraft_doc::{
    Document, ImageBlob, ImageObject, Node, NodeId, NodeKind, OpacityMask,
    cmyk::{Inks, SpotInks},
};

pub(super) struct Replacement {
    id: NodeId,
    blob: ImageBlob,
    alpha: Option<ImageBlob>,
}

pub(super) fn prepare(d: &Document, targets: &[NodeId], paint: &Paint) -> Result<Vec<Replacement>> {
    let mut out = Vec::new();
    for id in targets {
        crate::cancel::check()?;
        let Some(Node { kind: NodeKind::Image(im), .. }) = d.node(*id) else { continue };
        let source = d.images.get(&im.key).ok_or_else(|| error("image fill: missing image data"))?;
        let (w, h, alpha) = if source.is_cmyk() {
            let pixels = source.cmyk().ok_or_else(|| error("image fill: unreadable CMYK samples"))?;
            (pixels.width, pixels.height, None)
        } else {
            let pixels = source.decoded().ok_or_else(|| error("image fill: unreadable RGB/Gray samples"))?;
            let alpha = if pixels.pixels().any(|p| p.0[3] != 255) {
                let mut gray = image::GrayImage::new(pixels.width(), pixels.height());
                for (dst, src) in gray.pixels_mut().zip(pixels.pixels()) {
                    dst.0[0] = src.0[3];
                }
                let mut bytes = std::io::Cursor::new(Vec::new());
                gray.write_to(&mut bytes, image::ImageFormat::Png).map_err(error)?;
                Some(ImageBlob::png(bytes.into_inner()))
            } else {
                None
            };
            (pixels.width(), pixels.height(), alpha)
        };
        let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
        let (process, spot) = match paint {
            Paint::None => ([0; 4], None),
            Paint::Solid { swatch: Some(_), tint, .. } if *tint == 0.0 => ([0; 4], None),
            Paint::Solid { swatch: Some(name), tint, .. } if d.swatch(name).is_some_and(|s| s.spot) => ([0; 4], Some((name.clone(), byte(*tint)))),
            Paint::Solid { color, swatch: None, .. } => (profiles::cmyk(*color, profiles::document_cms(d).as_ref()).map(byte), None),
            _ => return Err(error("image fill replacement requires a process or spot flat paint")),
        };
        let count = usize::try_from(u64::from(w) * u64::from(h)).map_err(error)?;
        if count as u64 > vectorcraft_doc::cmyk::MAX_CMYK_PIXELS {
            return Err(error("image fill exceeds the decoded image limit"));
        }
        let samples = process.repeat(count);
        let inks = Inks::new(w, h, samples).ok_or_else(|| error("image fill exceeds the decoded image limit"))?;
        let blob = if let Some((name, tint)) = spot {
            let spots = SpotInks::new(w, h, vec![name], vec![tint; count]).ok_or_else(|| error("invalid image fill channels"))?;
            ImageBlob::cmyk_spot_tiff(&inks, &spots)
        } else {
            ImageBlob::cmyk_tiff(&inks)
        }
        .ok_or_else(|| error("could not encode image fill"))?;
        out.push(Replacement { id: *id, blob, alpha });
    }
    Ok(out)
}

pub(super) fn apply(d: &mut Document, replacements: Vec<Replacement>) -> Result<()> {
    for replacement in replacements {
        let key = replacement.blob.content_key();
        d.images.insert(key.clone(), replacement.blob);
        let alpha = replacement.alpha.map(|blob| {
            let key = blob.content_key();
            d.images.insert(key.clone(), blob);
            key
        });
        // A separate inner alpha mask preserves intrinsic coverage. The outer node keeps
        // its original opacity, clipping, mask, effects, metadata and document position.
        let child_id = d.alloc_id();
        let mask_id = d.alloc_id();
        let n = d.node_mut(replacement.id).ok_or_else(|| error("image disappeared during fill replacement"))?;
        let NodeKind::Image(im) = &mut n.kind else { return Err(error("image changed during fill replacement")) };
        im.key = key;
        im.link = None;
        n.appearance.items.retain(|item| !matches!(item, vectorcraft_doc::AppearanceItem::Fill(_)));
        if let Some(alpha) = alpha {
            let mut child = Node::new(child_id, NodeKind::Image(im.clone()));
            let mask = Node::new(mask_id, NodeKind::Image(Box::new(ImageObject { key: alpha, ..im.as_ref().clone() })));
            child.extras_mut().mask = Some(Box::new(OpacityMask::new(mask, true)));
            n.kind = NodeKind::Group { children: vec![std::sync::Arc::new(child)], clip: false };
        }
    }
    Ok(())
}

/// Trace printed raster coverage on the 600 dpi output grid. Pixel-cell boundaries are
/// fitted within half a pixel on detailed contours, without hole filling or implicit choke. Intrinsic alpha
/// and external opacity masks are applied by the renderer before the 0.005 threshold.
pub(super) fn footprint(d: &Document, n: &Node) -> Result<vectorcraft_geom::PathData> {
    trace_footprint(d, n, false)
}

/// Only opaque samples fully knock out paint below, even when they contain no ink.
/// Partially transparent samples leave underlying ink visible.
pub(super) fn opacity_footprint(d: &Document, n: &Node) -> Result<vectorcraft_geom::PathData> {
    trace_footprint(d, n, true)
}

fn trace_footprint(d: &Document, n: &Node, knockout: bool) -> Result<vectorcraft_geom::PathData> {
    use std::sync::Arc;
    use vectorcraft_geom::{PathData, Rect};
    use vectorcraft_render::{AntiAlias, RenderOptions, Renderer};
    const SCALE: f64 = 600. / 72.;
    if n.blend != vectorcraft_color::BlendMode::Normal
        || n.knockout == vectorcraft_doc::Knockout::On
        || n.appearance.effects.iter().any(|e| !e.is_noop())
    {
        return Err(error("Raster footprint requires Normal blending without live effects or knockout groups"));
    }
    let NodeKind::Image(im) = &n.kind else {
        return Err(error("Raster footprint requires an image"));
    };
    let blob = d.images.get(&im.key).ok_or_else(|| error("Raster footprint: image data is missing"))?;
    let coverage = if knockout { blob.clone() } else { coverage_blob(d, blob)? };
    let Some(b) = n.visual_bounds() else {
        return Ok(PathData::default());
    };
    let region =
        Rect::new((b.x0 * SCALE).floor() / SCALE, (b.y0 * SCALE).floor() / SCALE, (b.x1 * SCALE).ceil() / SCALE, (b.y1 * SCALE).ceil() / SCALE);
    let (w, h) = vectorcraft_render::raster_size(region, SCALE).map_err(error)?;
    if u64::from(w) * u64::from(h) > 150_000_000 {
        return Err(error("Raster footprint exceeds the 150-million-pixel limit (A2 at 600 dpi)"));
    }
    let mut source = d.clone();
    let key = coverage.content_key();
    source.images.insert(key.clone(), coverage);
    let mut silhouette = n.clone();
    if let NodeKind::Image(image) = &mut silhouette.kind {
        image.key = key;
        image.link = None;
    }
    source.layers = vec![Arc::new(silhouette)];
    source.drop_lookup_cache();
    let mut renderer = Renderer::new();
    let opts = RenderOptions {
        background: None,
        artboards: false,
        anti_alias: AntiAlias::None,
        smooth_images: false,
        image_mips: false,
        ..Default::default()
    };
    let rows = (4_000_000 / w.max(1) as usize).max(1);
    let mut path = PathData::default();
    for y in (0..h as usize).step_by(rows) {
        crate::cancel::check()?;
        let end = (y + rows).min(h as usize);
        let band = Rect::new(region.x0, region.y0 + y as f64 / SCALE, region.x1, region.y0 + end as f64 / SCALE);
        let decoded = vectorcraft_render::decode_failures();
        let rgba = renderer.render_region_with(&source, band, SCALE, &opts);
        if let Some(reason) = vectorcraft_render::decode_failure_reason(decoded) {
            return Err(error(reason));
        }
        if rgba.pixels.len() != w as usize * (end - y) * 4 {
            return Err(error("Raster footprint: incomplete image render"));
        }
        let mask: Vec<_> =
            rgba.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| {
                    if knockout {
                        p[3] == 255
                    } else {
                        vectorcraft_doc::inks::present(f32::from(p[3]) / 255., vectorcraft_doc::inks::PRESENCE_EPSILON)
                    }
                })
                .collect();
        path.subpaths.extend(super::super::sep_trap::outline(&mask, w as usize, end - y, band, false)?.subpaths);
        if path.subpaths.iter().map(|p| p.anchors.len()).sum::<usize>() > 1_000_000 {
            return Err(error("Raster footprint exceeds one million outline points; simplify the image or clip it before separation"));
        }
    }
    let path = vectorcraft_pathops::try_normalize(&path, vectorcraft_geom::FillRule::NonZero).map_err(error)?;
    // Pixel stair steps make round offsets needlessly expensive. Fit detailed rings to
    // half an output pixel; retain short rings (including tiny counters) exactly.
    let mut smooth = PathData::default();
    for ring in &path.subpaths {
        crate::cancel::check()?;
        let one = PathData::new(vec![ring.clone()]);
        let fitted = if ring.anchors.len() > 16 {
            vectorcraft_pathops::simplify_with(
                &one,
                &vectorcraft_pathops::SimplifyOptions { tolerance: 0.5 / SCALE, corner_angle_deg: 180.0, straight_lines: false },
            )
        } else {
            one.clone()
        };
        if fitted.subpaths.len() == 1 && fitted.subpaths.first().is_some_and(|p| p.anchors.len() >= 3) {
            smooth.subpaths.extend(fitted.subpaths);
        } else {
            smooth.subpaths.extend(one.subpaths);
        }
    }
    preserve_regions(path, smooth)
}

// Validate fitting on disconnected regions independently. A fragile tiny counter
// must not force every other glyph on a sheet back to hundreds of pixel steps.
fn preserve_regions(original: vectorcraft_geom::PathData, fitted: vectorcraft_geom::PathData) -> Result<vectorcraft_geom::PathData> {
    use std::collections::BTreeMap;
    use vectorcraft_geom::{FillRule, PathData};
    let n = original.subpaths.len();
    if n != fitted.subpaths.len() || n.saturating_mul(n) > 100_000_000 {
        return Ok(original);
    }
    let mut bounds = Vec::with_capacity(n);
    for (a, b) in original.subpaths.iter().zip(&fitted.subpaths) {
        let Some(a) = PathData::new(vec![a.clone()]).bounds() else {
            return Ok(original);
        };
        let Some(b) = PathData::new(vec![b.clone()]).bounds() else {
            return Ok(original);
        };
        bounds.push(a.union(b).inflate(2. * vectorcraft_pathops::DEFAULT_PRECISION, 2. * vectorcraft_pathops::DEFAULT_PRECISION));
    }
    let mut parents: Vec<usize> = (0..n).collect();
    let mut sizes = vec![1usize; n];
    fn root(parents: &[usize], mut i: usize) -> usize {
        while let Some(&p) = parents.get(i) {
            if p == i {
                break;
            }
            i = p;
        }
        i
    }
    for (i, a) in bounds.iter().enumerate() {
        crate::cancel::check()?;
        for (j, b) in bounds.iter().take(i).enumerate() {
            if a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1 {
                let (mut r, mut t) = (root(&parents, i), root(&parents, j));
                if r != t {
                    if sizes.get(r) < sizes.get(t) {
                        std::mem::swap(&mut r, &mut t);
                    }
                    let combined = sizes.get(r).copied().unwrap_or(0) + sizes.get(t).copied().unwrap_or(0);
                    if let Some(p) = parents.get_mut(t) {
                        *p = r;
                    }
                    if let Some(size) = sizes.get_mut(r) {
                        *size = combined;
                    }
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, (PathData, PathData)> = BTreeMap::new();
    for (i, (a, b)) in original.subpaths.into_iter().zip(fitted.subpaths).enumerate() {
        let (orig, fit) = groups.entry(root(&parents, i)).or_default();
        orig.subpaths.push(a);
        fit.subpaths.push(b);
    }
    let signature = |p: &PathData| (p.subpaths.len(), p.subpaths.iter().filter(|s| s.signed_area() < 0.).count());
    let mut out = PathData::default();
    for (orig, fit) in groups.into_values() {
        crate::cancel::check()?;
        // Fitting is optional. A failed fit or changed island/counter count keeps
        // the already normalized original for this entire connected region.
        let chosen = match vectorcraft_pathops::try_normalize(&fit, FillRule::NonZero) {
            Ok(p) if signature(&p) == signature(&orig) => p,
            _ => orig,
        };
        out.subpaths.extend(chosen.subpaths);
    }
    Ok(out)
}

// Store actual ink coverage in alpha so the renderer composes it with the object's
// opacity and mask before thresholding. Process white is paper; a white spot is ink.
fn coverage_blob(d: &Document, blob: &ImageBlob) -> Result<ImageBlob> {
    let cmyk = if blob.is_cmyk() { Some(blob.cmyk().ok_or_else(|| error("Raster footprint: unreadable CMYK samples"))?) } else { None };
    let rgb = if cmyk.is_none() { Some(blob.decoded().ok_or_else(|| error("Raster footprint: unreadable RGB/Gray samples"))?) } else { None };
    let spots = blob.spot_inks().map_err(error)?;
    let (w, h) = if let Some(c) = &cmyk {
        (c.width, c.height)
    } else if let Some(r) = &rgb {
        r.dimensions()
    } else {
        return Err(error("Raster footprint has no samples"));
    };
    let cms = profiles::document_cms(d);
    let mut cache = std::collections::HashMap::new();
    let mut mask = image::RgbaImage::new(w, h);
    for (i, pixel) in mask.pixels_mut().enumerate() {
        if i % 16384 == 0 {
            crate::cancel::check()?;
        }
        let (mut coverage, alpha) = if let Some(c) = &cmyk {
            (c.data.get(i * 4..i * 4 + 4).unwrap_or_default().iter().copied().max().unwrap_or(0) as f32 / 255., 1.)
        } else if let Some(r) = &rgb {
            let p = r.as_raw().get(i * 4..i * 4 + 4).ok_or_else(|| error("Raster footprint: incomplete image samples"))?;
            let [red, green, blue, alpha] = <[u8; 4]>::try_from(p).map_err(error)?;
            let coverage = *cache.entry([red, green, blue]).or_insert_with(|| {
                profiles::cmyk(vectorcraft_color::Color::rgb(red as f32 / 255., green as f32 / 255., blue as f32 / 255.), cms.as_ref())
                    .into_iter()
                    .fold(0.0_f32, f32::max)
            });
            if cache.len() >= 65536 {
                cache.clear();
            }
            (coverage, alpha as f32 / 255.)
        } else {
            (0., 0.)
        };
        if let Some(s) = &spots {
            let count = s.names.len();
            coverage = coverage.max(s.data.get(i * count..i * count + count).unwrap_or_default().iter().copied().max().unwrap_or(0) as f32 / 255.);
        }
        *pixel = image::Rgba([255, 255, 255, (coverage.clamp(0., 1.) * alpha * 255.).round() as u8]);
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    mask.write_to(&mut bytes, image::ImageFormat::Png).map_err(error)?;
    Ok(ImageBlob::png(bytes.into_inner()))
}

/// Per-sample trap permission retains the raster's intrinsic alpha and external masks.
pub(in crate::cmd) fn trap_eligibility(d: &Document, im: &ImageObject, threshold: f32, permission: bool) -> Result<ImageBlob> {
    let blob = d.images.get(&im.key).ok_or_else(|| error("Trap: image data is missing"))?;
    if blob.spot_names().iter().any(|name| name.contains("NEON")) {
        return Err(error("Trap does not support neon image channels"));
    }
    let cmyk = if blob.is_cmyk() { Some(blob.cmyk().ok_or_else(|| error("Trap: unreadable CMYK image"))?) } else { None };
    let rgb = if cmyk.is_none() { Some(blob.decoded().ok_or_else(|| error("Trap: unreadable RGB image"))?) } else { None };
    let spots = blob.spot_inks().map_err(error)?;
    let (w, h) = if let Some(c) = &cmyk {
        (c.width, c.height)
    } else if let Some(r) = &rgb {
        r.dimensions()
    } else {
        return Err(error("Trap: missing image samples"));
    };
    let cms = profiles::document_cms(d);
    let mut cache = std::collections::HashMap::new();
    let mut output = image::RgbaImage::new(w, h);
    for (i, pixel) in output.pixels_mut().enumerate() {
        if i % 16384 == 0 {
            crate::cancel::check()?;
        }
        let (ink, alpha) = if let Some(c) = &cmyk {
            let raw = c.data.get(i * 4..i * 4 + 4).ok_or_else(|| error("Trap: incomplete CMYK samples"))?;
            (<[u8; 4]>::try_from(raw).map_err(error)?.map(|v| v as f32 / 255.), 255)
        } else if let Some(r) = &rgb {
            let raw = r.as_raw().get(i * 4..i * 4 + 4).ok_or_else(|| error("Trap: incomplete RGB samples"))?;
            let [red, green, blue, alpha] = <[u8; 4]>::try_from(raw).map_err(error)?;
            let ink = *cache.entry([red, green, blue]).or_insert_with(|| {
                profiles::cmyk(vectorcraft_color::Color::rgb(red as f32 / 255., green as f32 / 255., blue as f32 / 255.), cms.as_ref())
            });
            if cache.len() >= 65536 {
                cache.clear();
            }
            (ink, alpha)
        } else {
            ([0.; 4], 0)
        };
        let mut present = ink.iter().any(|v| vectorcraft_doc::inks::present(*v, vectorcraft_doc::inks::PRESENCE_EPSILON));
        let mut black = ink[3];
        if let Some(s) = &spots {
            let count = s.names.len();
            for (name, v) in s.names.iter().zip(s.data.get(i * count..i * count + count).unwrap_or_default()) {
                let tint = *v as f32 / 255.;
                if vectorcraft_doc::inks::present(tint, vectorcraft_doc::inks::PRESENCE_EPSILON) {
                    present = true;
                    black = black.max(
                        vectorcraft_doc::inks::spot_color(d, name)
                            .ok_or_else(|| error(format!("Trap: missing alternate colour for {name}")))?
                            .to_cmyk()[3],
                    );
                }
            }
        }
        let v = if permission && present && black < threshold { 255 } else { 0 };
        *pixel = image::Rgba([v, v, v, alpha]);
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    output.write_to(&mut bytes, image::ImageFormat::Png).map_err(error)?;
    Ok(ImageBlob::png(bytes.into_inner()))
}

#[cfg(test)]
mod smoothing_tests {
    use super::*;
    use vectorcraft_geom::{BezPath, PathData, Rect, shapes};
    #[test]
    fn a_failed_counter_fit_does_not_discard_a_distant_valid_fit() {
        let mut outer = shapes::rectangle(Rect::new(0., 0., 10., 10.));
        let mut hole = shapes::rectangle(Rect::new(2., 2., 3., 3.));
        hole.reverse();
        outer.subpaths.extend(hole.subpaths);
        let mut detailed = BezPath::new();
        detailed.move_to((20., 0.));
        for x in 1..=10 {
            detailed.line_to((20. + x as f64, 0.));
        }
        for y in 1..=10 {
            detailed.line_to((30., y as f64));
        }
        for x in (0..10).rev() {
            detailed.line_to((20. + x as f64, 10.));
        }
        for y in (1..10).rev() {
            detailed.line_to((20., y as f64));
        }
        detailed.close_path();
        let mut original = outer.clone();
        original.subpaths.extend(PathData::from_bezpath(&detailed).subpaths);
        let mut fitted = shapes::rectangle(Rect::new(0., 0., 10., 10.));
        let mut moved_hole = shapes::rectangle(Rect::new(11., 2., 12., 3.));
        moved_hole.reverse();
        fitted.subpaths.extend(moved_hole.subpaths);
        fitted.subpaths.extend(shapes::rectangle(Rect::new(20., 0., 30., 10.)).subpaths);
        let out = preserve_regions(original, fitted).unwrap();
        assert_eq!(out.subpaths.len(), 3);
        assert_eq!(out.subpaths.iter().map(|s| s.anchors.len()).sum::<usize>(), 12);
        assert!(out.subpaths.iter().any(|s| s.signed_area() < 0.), "the original counter survives");
    }
}
