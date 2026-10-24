//! Experimental external-edge trap. The explicit K threshold supports measurement; 0.7 is the
//! accepted separation rule. Uses the same plate renderer as headless screenshots.
use std::sync::Arc;

use serde_json::{Value, json};
use vectorcraft_color::{Color, Paint};
use vectorcraft_doc::{Appearance, Document, ImageBlob, ImageObject, LayerColor, Node, NodeKind, OpacityMask};
use vectorcraft_geom::{Affine, BezPath, PathData, Rect};
use vectorcraft_render::{AntiAlias, RenderOptions, Renderer, proof::ProofSetup};

use super::{CommandSpec, Session, bad, cmd, has_doc};
use crate::Result;

mod bands;
mod colour;
mod index;
mod pixels;
mod raster;
pub(super) use raster::rasterize;
#[cfg(test)]
mod tests;
mod union;
mod vector;

const C: &str = "sep.trap";
const SCALE: f64 = 600.0 / 72.0;
const MM: f64 = 72.0 / 25.4;
// A2 at 600 dpi is ~139.3 MP; include the trap margin, but bound pathological jobs.
const MAX_PIXELS: usize = 150_000_000;
const BAND_PIXELS: usize = 4_000_000;

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "sep.trap",
        "Trap…",
        [],
        None,
        "{blackThreshold: K coverage 0..1 (required experimental setting)} replace Auto Trap-1 from Artwork only: 600 dpi, 0.2 mm outward external band, nearest art ink samples, 0.01 mm spread/choke. Accepted K threshold: 0.7; vector art only → {pixels, width, height, blackThreshold, seconds}",
        has_doc,
        run
    )]
}

// NameRule is case-sensitive. These special inks are outside this experiment's scope.
// WHP's exact NO WHP / NO WHP OR POWDER exclusions affect underprint, not trap tags.
fn specialty_name(name: &str) -> bool {
    name.contains("NEON")
}

const INK_EPSILON: f32 = vectorcraft_doc::inks::PRESENCE_EPSILON;

fn paint_ink(doc: &Document, color: &Color, swatch: Option<&str>, tint: f32, threshold: f32) -> (vectorcraft_doc::inks::Inks, bool) {
    let ink = vectorcraft_doc::inks::inks(doc, &vectorcraft_color::cms::active(), color, swatch.map(|name| (name, tint)), Default::default());
    // PAL Any / NotUsing: a spot's tint, not its alternate, is its actual coverage.
    let no_ink = ink.cmyk.iter().all(|v| !vectorcraft_doc::inks::present(*v, INK_EPSILON))
        && ink.spot.as_ref().is_none_or(|(_, tint)| !vectorcraft_doc::inks::present(*tint, INK_EPSILON));
    let alternate = swatch.and_then(|name| vectorcraft_doc::inks::spot_color(doc, name));
    let k = alternate.unwrap_or(*color).to_cmyk()[3];
    (ink, !no_ink && k < threshold)
}

// A CMYK ramp is a convex combination of its stops. These bounds prove that the
// existing epsilon/K predicate is constant over the whole ramp; crossing ramps
// remain unsupported rather than interpolating a Boolean eligibility decision.
fn gradient_eligibility(g: &vectorcraft_color::gradient::GradientPaint, threshold: f32) -> Option<bool> {
    use vectorcraft_color::gradient::GradientKind;
    if !matches!(g.gradient.kind, GradientKind::Linear | GradientKind::Radial)
        || g.swatch.is_some()
        || g.freeform.is_some()
        || g.gradient.stops.is_empty()
    {
        return None;
    }
    let mut low = [1.0_f32; 4];
    let mut high = [0.0_f32; 4];
    for stop in &g.gradient.stops {
        if stop.opacity != 1.0 || stop.swatch.is_some() {
            return None;
        }
        let Color::Cmyk { c, m, y, k } = stop.color else {
            return None;
        };
        for ((lo, hi), v) in low.iter_mut().zip(&mut high).zip([c, m, y, k]) {
            if !(0.0..=1.0).contains(&v) {
                return None;
            }
            *lo = lo.min(v);
            *hi = hi.max(v);
        }
    }
    if low[3] >= threshold || high.iter().all(|v| !vectorcraft_doc::inks::present(*v, INK_EPSILON)) {
        Some(false)
    } else if high[3] < threshold && low.iter().any(|v| vectorcraft_doc::inks::present(*v, INK_EPSILON)) {
        Some(true)
    } else {
        None
    }
}

fn eligibility(n: &mut Node, doc: &Document, threshold: f32, inherited: bool, images: &mut Vec<(String, ImageBlob)>) -> Result<()> {
    if n.name.as_deref().is_some_and(specialty_name) {
        return Err(bad(C, "prototype does not support neon artwork"));
    }
    if !matches!(n.kind, NodeKind::Path { .. } | NodeKind::Compound { .. } | NodeKind::Group { .. } | NodeKind::Layer { .. } | NodeKind::Image(_)) {
        return Err(bad(C, "Trap supports paths, groups and raster artwork; this object type must be expanded first"));
    }
    let permission = n.extras().trap_external.unwrap_or(inherited);
    if let NodeKind::Image(im) = &mut n.kind {
        let blob = super::sep::images::trap_eligibility(doc, im, threshold, permission)?;
        let key = blob.content_key();
        im.key = key.clone();
        im.link = None;
        images.push((key, blob));
    }
    // Recolour each object's own paints, retaining its geometry, clipping, opacity and stacking.
    for item in &mut n.appearance.items {
        let paint = match item {
            vectorcraft_doc::AppearanceItem::Fill(f) => &mut f.paint,
            vectorcraft_doc::AppearanceItem::Stroke(s) => &mut s.paint,
        };
        if let Paint::Gradient(g) = paint {
            let on = gradient_eligibility(g, threshold)
                .ok_or_else(|| bad(C, "prototype requires an opaque CMYK gradient with uniform ink/black eligibility"))?;
            *paint = Paint::solid(if on && permission { Color::WHITE } else { Color::BLACK });
            continue;
        }
        if !matches!(paint, Paint::None | Paint::Solid { .. }) {
            return Err(bad(C, "prototype eligibility requires solid paints or supported CMYK gradients"));
        }
        if let Paint::Solid { swatch, .. } = paint
            && swatch.as_deref().is_some_and(specialty_name)
        {
            return Err(bad(C, "prototype does not support neon inks"));
        }
        vectorcraft_doc::inks::visit_paint(paint, &mut |color, swatch, tint| {
            let (_, on) = paint_ink(doc, color, swatch.as_deref(), *tint, threshold);
            *color = if on && permission { Color::WHITE } else { Color::BLACK };
            *swatch = None;
            *tint = 1.0;
        });
    }
    if let Some(children) = n.children_mut() {
        for child in children {
            eligibility(Arc::make_mut(child), doc, threshold, permission, images)?;
        }
    }
    Ok(())
}

fn tag_conflicts(n: &Node, doc: &Document, threshold: f32, inherited: Option<bool>, counts: &mut [usize; 2]) {
    let tag = n.extras().trap_external.or(inherited);
    for item in &n.appearance.items {
        let paint = match item {
            vectorcraft_doc::AppearanceItem::Fill(f) if f.visible && f.opacity > 0.0 => &f.paint,
            vectorcraft_doc::AppearanceItem::Stroke(s) if s.visible && s.opacity > 0.0 && s.width > 0.0 => &s.paint,
            _ => continue,
        };
        let eligible = match paint {
            Paint::Solid { color, swatch, tint } => Some(paint_ink(doc, color, swatch.as_deref(), *tint, threshold).1),
            Paint::Gradient(g) => gradient_eligibility(g, threshold),
            _ => None,
        };
        match (tag, eligible) {
            (Some(true), Some(false)) => counts[0] += 1,
            (Some(false), Some(true)) => counts[1] += 1,
            _ => (),
        }
    }
    if let Some(cs) = n.children() {
        for c in cs {
            tag_conflicts(c, doc, threshold, tag, counts);
        }
    }
}

fn has_render_extras(n: &Node) -> bool {
    let mut extras = n.extras().clone();
    extras.trap_external = None;
    // Artist intent is metadata, just like trap permission; it cannot switch rendering paths.
    extras.sep_mark = None;
    extras != Default::default()
}

fn options(plate: Option<&str>) -> RenderOptions {
    RenderOptions {
        background: plate.map(|_| [255; 4]),
        artboards: false,
        anti_alias: AntiAlias::None,
        smooth_images: false,
        image_mips: false,
        overprint_preview: true,
        proof: plate.map(|p| ProofSetup { separations: Some(vec![p.into()]), ..Default::default() }),
        ..Default::default()
    }
}

pub(super) fn outline(mask: &[bool], w: usize, h: usize, region: Rect, cleanup: bool) -> Result<PathData> {
    outline_with_budget(mask, w, h, region, cleanup, MAX_CONTOUR_ANCHORS)
}

// Reserve at most the existing ink working allowance for contour/path storage.
// 1 KiB per anchor conservatively covers simultaneously live Loop, Component,
// BezPath and PathData allocations, geometric Vec growth and per-loop overhead.
// Grid scratch is separately bounded by MAX_PIXELS, as in rendering.
const MAX_CONTOUR_ANCHORS: usize = (vectorcraft_doc::cmyk::spots::MAX_INK_BYTES / 1024) as usize;

fn check_contour_budget(mask: &[bool], w: usize, h: usize, limit: usize) -> Result<()> {
    if w == 0 || h == 0 || w.checked_mul(h) != Some(mask.len()) || mask.len() > MAX_PIXELS {
        return Err(bad(C, "invalid contour grid"));
    }
    // Count turns without allocating components or contours. At a grid corner,
    // one/three filled neighbours make one turn; diagonal pairs make two.
    // Each loop has at least four turns. Allow one extra start vertex per loop
    // (the tracer retains its starting point even on a straight edge).
    let mut turns = 0usize;
    for y in 0..=h {
        crate::cancel::check()?;
        let mut nw = false;
        let mut sw = false;
        for x in 0..=w {
            let ne = y > 0 && x < w && mask.get((y - 1) * w + x) == Some(&true);
            let se = y < h && x < w && mask.get(y * w + x) == Some(&true);
            let count = usize::from(nw) + usize::from(ne) + usize::from(sw) + usize::from(se);
            turns += match count {
                1 | 3 => 1,
                2 if nw == se => 2,
                _ => 0,
            };
            if turns + turns / 4 > limit {
                return Err(bad(
                    C,
                    format!(
                        "Raster contour/anchor memory budget exceeded ({limit} anchors); simplify the alpha or clip the artwork before separation"
                    ),
                ));
            }
            nw = ne;
            sw = se;
        }
    }
    Ok(())
}

fn outline_with_budget(mask: &[bool], w: usize, h: usize, region: Rect, cleanup: bool, limit: usize) -> Result<PathData> {
    // Must precede trace_mask: even its component tables can explode on noise.
    check_contour_budget(mask, w, h, limit)?;
    let mut path = BezPath::new();
    for component in vectorcraft_trace::trace_mask(mask, w, h) {
        for contour in std::iter::once(component.outer).chain(component.holes) {
            let mut points = contour.pts.into_iter();
            if let Some((x, y)) = points.next() {
                path.move_to((region.x0 + f64::from(x) / SCALE, region.y0 + f64::from(y) / SCALE));
                for (x, y) in points {
                    path.line_to((region.x0 + f64::from(x) / SCALE, region.y0 + f64::from(y) / SCALE));
                }
                path.close_path();
            }
        }
    }
    let path = PathData::from_bezpath(&path);
    if !cleanup {
        return Ok(path);
    }
    log::debug!("trap: cleaning {} anchors", path.subpaths.iter().map(|p| p.anchors.len()).sum::<usize>());
    let spread = vectorcraft_pathops::try_offset_path(&path, 0.01 * MM, vectorcraft_pathops::Join::Miter, 4.0)
        .map_err(|e| bad(C, format!("clip spread: {e}")))?;
    vectorcraft_pathops::try_offset_path(&spread, -0.01 * MM, vectorcraft_pathops::Join::Miter, 4.0).map_err(|e| bad(C, format!("clip choke: {e}")))
}

fn image_node(d: &mut Document, key: String, w: u32, h: u32, xf: Affine) -> Node {
    Node::new(d.alloc_id(), NodeKind::Image(Box::new(ImageObject { key, width: w, height: h, xf, link: None, placement: Default::default() })))
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    build(s, p, false)
}

pub(super) fn trap_task(s: &mut Session) -> Result<Value> {
    build(s, &json!({"blackThreshold":0.7}), true)
}

fn build(s: &mut Session, p: &Value, pre_cleanup: bool) -> Result<Value> {
    crate::cancel::check()?;
    #[cfg(not(target_arch = "wasm32"))]
    let started = std::time::Instant::now();
    #[cfg(not(target_arch = "wasm32"))]
    let elapsed = || Some(started.elapsed().as_secs_f64());
    #[cfg(target_arch = "wasm32")]
    let elapsed = || None::<f64>;
    let threshold = p
        .get("blackThreshold")
        .and_then(Value::as_f64)
        .filter(|x| (0.0..=1.0).contains(x))
        .ok_or_else(|| bad(C, "blackThreshold must be an explicit experimental K coverage in 0..1"))?;
    let doc = &s.doc()?.doc;
    let art: Vec<_> = doc.layers.iter().filter(|n| n.name.as_deref() == Some("Artwork")).cloned().collect();
    if art.len() != 1 {
        return Err(bad(C, "requires exactly one top-level Artwork layer"));
    }
    let mut source = (**doc).clone();
    source.layers = art;
    // Match the player's preserve-numbers PDF output: keep existing CMYK/spot inks,
    // convert RGB process paints (including gradient stops) through the assigned profile.
    let cms = super::sep::profiles::document_cms(&source);
    let spots: std::collections::BTreeSet<_> = source.swatches.iter().filter(|w| w.spot).map(|w| w.name.clone()).collect();
    for layer in &mut source.layers {
        vectorcraft_doc::inks::visit_node_colors(Arc::make_mut(layer), &mut |color, swatch, _| {
            if !matches!(color, Color::Cmyk { .. }) && !swatch.as_ref().is_some_and(|s| spots.contains(s)) {
                let [c, m, y, k] = super::sep::profiles::cmyk(*color, cms.as_ref());
                *color = Color::cmyk(c, m, y, k);
            }
        });
    }
    source.drop_lookup_cache();
    let bounds = source.art_bounds().ok_or_else(|| bad(C, "Artwork has no bounds"))?.inflate(0.3 * MM, 0.3 * MM);
    let region = Rect::new(
        (bounds.x0 * SCALE).floor() / SCALE,
        (bounds.y0 * SCALE).floor() / SCALE,
        (bounds.x1 * SCALE).ceil() / SCALE,
        (bounds.y1 * SCALE).ceil() / SCALE,
    );
    let (w, h) = vectorcraft_render::raster_size(region, SCALE).map_err(|e| bad(C, e.to_string()))?;
    let n = w as usize * h as usize;
    if n > MAX_PIXELS {
        return Err(bad(C, format!("prototype grid {w} x {h} exceeds the 150-million-pixel limit (A2 at 600 dpi)")));
    }
    if !pre_cleanup && n > 64_000_000 {
        return Err(bad(C, "Standalone trap exceeds the single-image limit; use the PAL TrapTask for banded transfers up to A2 at 600 dpi"));
    }
    let mut conflicts = [0; 2];
    for layer in &source.layers {
        tag_conflicts(layer, &source, threshold as f32, None, &mut conflicts);
    }
    let conflicts = json!({"taggedPaintsExcludedByInkRules":conflicts[0],"inkEligiblePaintsExcludedByTags":conflicts[1]});
    let mut eligible = source.clone();
    let mut eligible_images = Vec::new();
    for layer in &mut eligible.layers {
        eligibility(Arc::make_mut(layer), &source, threshold as f32, true, &mut eligible_images)?;
    }
    eligible.images.extend(eligible_images);
    let mut renderer = Renderer::new();
    let silhouette = union::artwork(&source)?.map(|path| {
        let mut silhouette = source.clone();
        let shape = Node::path(silhouette.alloc_id(), path, Appearance::basic(Paint::solid(Color::BLACK), Paint::None, 0.0));
        silhouette.layers = vec![Arc::new(shape)];
        silhouette.drop_lookup_cache();
        silhouette
    });
    let plates = vectorcraft_doc::inks::plates(&source);
    let layer_name = if pre_cleanup { "Trap Layer 1" } else { "Auto Trap-1" };
    let existing: Vec<_> = doc.layers.iter().filter(|l| l.name.as_deref() == Some(layer_name)).map(|l| l.id).collect();
    if pre_cleanup && n > 64_000_000 {
        let mut runs = vector::Runs::default();
        let mut mask = vec![false; n];
        let mut next_row = 0;
        let mut count = 0;
        for band in bands::rows(w, h, region, 8) {
            crate::cancel::check()?;
            let pixels =
                render_band(&source, &eligible, silhouette.as_ref(), &mut renderer, band.region, w, band.height, threshold, band.first, band.last)?;
            count += pixels.mask.iter().filter(|&&v| v).count();
            let core = pixels.mask.get(band.first * w as usize..band.last * w as usize).ok_or_else(|| bad(C, "incomplete trap band"))?;
            let target = mask.get_mut(next_row * w as usize..next_row * w as usize + core.len()).ok_or_else(|| bad(C, "invalid trap band extent"))?;
            target.copy_from_slice(core);
            next_row += band.last - band.first;
            runs.add(vector::Band {
                mask: &pixels.mask,
                process: &pixels.process,
                spot: &pixels.spot,
                plates: &plates,
                width: w as usize,
                region: band.region,
            })?;
        }
        let clip = outline(&mask, w as usize, h as usize, region, false)?;
        if count > 0 {
            runs.insert(s, &plates, clip, &existing)?;
        } else {
            s.edit("Trap", |d, _| {
                d.remove_many(&existing)?;
                Ok(())
            })?;
        }
        return Ok(json!({"pixels":count,"width":w,"height":h,"blackThreshold":threshold,"seconds":elapsed(),"tagConflicts":conflicts}));
    }
    let RasterBand { mask, process, spot } =
        render_band(&source, &eligible, silhouette.as_ref(), &mut renderer, region, w, h, threshold, 0, h as usize)?;
    let count = mask.iter().filter(|&&v| v).count();
    if count == 0 {
        s.edit("Trap", |d, _| {
            d.remove_many(&existing)?;
            Ok(())
        })?;
        return Ok(json!({"pixels":0,"width":w,"height":h,"blackThreshold":threshold,"seconds":elapsed(),"tagConflicts":conflicts}));
    }
    let clip = outline(&mask, w as usize, h as usize, region, !pre_cleanup)?;
    if clip.is_empty() {
        return Err(bad(C, "clip cleanup unexpectedly removed the band"));
    }
    let spot_count = plates.len().saturating_sub(4);
    if pre_cleanup {
        let mut runs = vector::Runs::default();
        runs.add(vector::Band { mask: &mask, process: &process, spot: &spot, plates: &plates, width: w as usize, region })?;
        runs.insert(s, &plates, clip, &existing)?;
        return Ok(json!({"pixels":count,"width":w,"height":h,"blackThreshold":threshold,"seconds":elapsed(),"tagConflicts":conflicts}));
    }
    let inks = vectorcraft_doc::cmyk::Inks::new(w, h, process).ok_or_else(|| bad(C, "invalid process samples"))?;
    let blob = if spot_count == 0 {
        ImageBlob::cmyk_tiff(&inks)
    } else {
        let spots = vectorcraft_doc::cmyk::SpotInks::new(w, h, plates.into_iter().skip(4).map(|p| p.name).collect(), spot)
            .ok_or_else(|| bad(C, "invalid spot samples"))?;
        ImageBlob::cmyk_spot_tiff(&inks, &spots)
    }
    .ok_or_else(|| bad(C, "could not encode ink image"))?;
    let alpha: Vec<_> = mask.iter().map(|&v| if v { 255 } else { 0 }).collect();
    let mut png = std::io::Cursor::new(Vec::new());
    image::GrayImage::from_raw(w, h, alpha)
        .ok_or_else(|| bad(C, "invalid mask"))?
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| bad(C, e.to_string()))?;
    let xf = Affine::translate((region.x0, region.y0)) * Affine::scale(1.0 / SCALE);
    crate::cancel::check()?;
    s.edit("Trap", |d, _| {
        d.remove_many(&existing)?;
        let key = super::menucmds::unique_key(d, "trap-inks");
        d.images.insert(key.clone(), blob);
        let mut image = image_node(d, key, w, h, xf);
        let key = super::menucmds::unique_key(d, "trap-alpha");
        d.images.insert(key.clone(), ImageBlob::new("image/png", png.into_inner()));
        let alpha = image_node(d, key, w, h, xf);
        image.extras_mut().mask = Some(Box::new(OpacityMask::new(alpha, true)));
        let mut clip = Node::path(d.alloc_id(), clip, Appearance::basic(Paint::None, Paint::None, 0.0));
        if let NodeKind::Path { clipping, .. } = &mut clip.kind {
            *clipping = true;
        }
        let mut group = Node::group(d.alloc_id(), vec![Arc::new(clip), Arc::new(image)]);
        if let NodeKind::Group { clip, .. } = &mut group.kind {
            *clip = true;
        }
        let mut layer = Node::layer(d.alloc_id(), layer_name, LayerColor::Preset(0));
        if let Some(children) = layer.children_mut() {
            children.push(Arc::new(group));
        }
        d.insert(None, if pre_cleanup { usize::MAX } else { 0 }, layer)?;
        Ok(())
    })?;
    Ok(json!({"pixels":count,"width":w,"height":h,"blackThreshold":threshold,"seconds":elapsed(),"tagConflicts":conflicts}))
}

struct RasterBand {
    mask: Vec<bool>,
    process: Vec<u8>,
    spot: Vec<u8>,
}
#[allow(clippy::too_many_arguments)]
fn render_band(
    source: &Document,
    eligible: &Document,
    silhouette: Option<&Document>,
    renderer: &mut Renderer,
    region: Rect,
    w: u32,
    h: u32,
    threshold: f64,
    first_row: usize,
    last_row: usize,
) -> Result<RasterBand> {
    let n = w as usize * h as usize;
    log::debug!("trap: rendering {w} x {h}");
    let decoded = vectorcraft_render::decode_failures();
    let rgba = renderer.render_region_with(source, region, SCALE, &options(None));
    if let Some(reason) = vectorcraft_render::decode_failure_reason(decoded) {
        return Err(bad(C, reason));
    }
    let decoded = vectorcraft_render::decode_failures();
    let eligible_rgba = renderer.render_region_with(eligible, region, SCALE, &options(None));
    if let Some(reason) = vectorcraft_render::decode_failure_reason(decoded) {
        return Err(bad(C, reason));
    }
    crate::cancel::check()?;
    if rgba.pixels.len() != n * 4 || eligible_rgba.pixels.len() != n * 4 {
        return Err(bad(C, "incomplete artwork render"));
    }
    let occupied: Vec<_> = rgba.pixels.as_chunks::<4>().0.iter().map(|[_, _, _, alpha]| *alpha >= 128).collect();
    let allowed: Vec<_> = eligible_rgba.pixels.as_chunks::<4>().0.iter().map(|[red, _, _, _]| *red >= 128).collect();
    drop(rgba);
    drop(eligible_rgba);
    log::debug!("trap: vector union");
    let union = if let Some(silhouette) = silhouette {
        let decoded = vectorcraft_render::decode_failures();
        let rgba = renderer.render_region_with(silhouette, region, SCALE, &options(None));
        if let Some(reason) = vectorcraft_render::decode_failure_reason(decoded) {
            return Err(bad(C, reason));
        }
        if rgba.pixels.len() != n * 4 {
            return Err(bad(C, "incomplete art union render"));
        }
        rgba.pixels.as_chunks::<4>().0.iter().map(|[_, _, _, a]| *a >= 128).collect()
    } else {
        log::debug!("trap: renderer occupancy with one-pixel seam closing");
        pixels::close_union(&occupied, w as usize, h as usize)
    };
    crate::cancel::check()?;
    log::debug!("trap: nearest features");
    let mut nearest = pixels::band(&occupied, &allowed, &union, w as usize, h as usize, 0.2 * 600.0 / 25.4)?;
    for (i, sample) in nearest.iter_mut().enumerate() {
        if i / (w as usize) < first_row || i / w as usize >= last_row {
            *sample = None;
        }
    }
    let colours = colour::samples(source, &mut nearest, w as usize, region, threshold as f32)?;
    let mask: Vec<_> = nearest.iter().map(Option::is_some).collect();

    log::debug!("trap: plate samples");
    let plates = vectorcraft_doc::inks::plates(source);
    if plates.len() > 32 {
        return Err(bad(C, "too many inks for DeviceN"));
    }
    let mut process = vec![0; n * 4];
    let spot_count = plates.len().saturating_sub(4);
    let mut spot = vec![0; n * spot_count];
    if let Some(colours) = colours {
        colours.write(&plates, &mut process, &mut spot);
    } else {
        for (channel, plate) in plates.iter().enumerate() {
            crate::cancel::check()?;
            let decoded = vectorcraft_render::decode_failures();
            let rendered = renderer.render_region_with(source, region, SCALE, &options(Some(&plate.name)));
            if let Some(reason) = vectorcraft_render::decode_failure_reason(decoded) {
                return Err(bad(C, reason));
            }
            if rendered.pixels.len() != n * 4 {
                return Err(bad(C, "incomplete plate render"));
            }
            for (i, sample) in nearest.iter().enumerate() {
                if let Some(sample) = sample {
                    let ink = 255 - rendered.pixels.get(*sample as usize * 4).copied().unwrap_or(255);
                    let out = if channel < 4 { process.get_mut(i * 4 + channel) } else { spot.get_mut(i * spot_count + channel - 4) };
                    if let Some(out) = out {
                        *out = ink;
                    }
                }
            }
        }
    }
    Ok(RasterBand { mask, process, spot })
}
