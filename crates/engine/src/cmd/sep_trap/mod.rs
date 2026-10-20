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

mod pixels;
#[cfg(test)]
mod tests;
mod union;

const C: &str = "sep.trap";
const SCALE: f64 = 600.0 / 72.0;
const MM: f64 = 72.0 / 25.4;
const MAX_PIXELS: usize = 64_000_000;

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
    name.contains("NEON") || name.starts_with("M-") || name.starts_with("DK M-")
}

const INK_EPSILON: f32 = 0.005;

fn eligibility(n: &mut Node, doc: &Document, threshold: f32) -> Result<()> {
    if n.name.as_deref().is_some_and(specialty_name) {
        return Err(bad(C, "prototype does not support metallic or neon artwork"));
    }
    if !matches!(n.kind, NodeKind::Path { .. } | NodeKind::Compound { .. } | NodeKind::Group { .. } | NodeKind::Layer { .. }) {
        return Err(bad(C, "prototype supports vector paths and groups only"));
    }
    // Recolour each object's own paints, retaining its geometry, clipping, opacity and stacking.
    for item in &mut n.appearance.items {
        let paint = match item {
            vectorcraft_doc::AppearanceItem::Fill(f) => &mut f.paint,
            vectorcraft_doc::AppearanceItem::Stroke(s) => &mut s.paint,
        };
        if !matches!(paint, Paint::None | Paint::Solid { .. }) {
            return Err(bad(C, "prototype eligibility requires solid vector paints"));
        }
        if let Paint::Solid { swatch, .. } = paint
            && swatch.as_deref().is_some_and(specialty_name)
        {
            return Err(bad(C, "prototype does not support metallic or neon inks"));
        }
        vectorcraft_doc::inks::visit_paint(paint, &mut |color, swatch, tint| {
            let ink = vectorcraft_doc::inks::inks(
                doc,
                &vectorcraft_color::cms::active(),
                color,
                swatch.as_deref().map(|name| (name, *tint)),
                Default::default(),
            );
            // PAL SeparationTypeRule Any / NotUsing, Epsilon 0.005. A spot's
            // coverage is its tint; its alternate is not a process separation.
            let no_ink = ink.cmyk.iter().all(|v| *v < INK_EPSILON) && ink.spot.as_ref().is_none_or(|(_, tint)| *tint < INK_EPSILON);
            let alternate = swatch.as_deref().and_then(|name| vectorcraft_doc::inks::spot_color(doc, name));
            let k = alternate.unwrap_or(*color).to_cmyk()[3];
            let off = no_ink || k >= threshold;
            *color = if off { Color::BLACK } else { Color::WHITE };
            *swatch = None;
            *tint = 1.0;
        });
    }
    if let Some(children) = n.children_mut() {
        for child in children {
            eligibility(Arc::make_mut(child), doc, threshold)?;
        }
    }
    Ok(())
}

fn options(plate: Option<&str>) -> RenderOptions {
    RenderOptions {
        background: plate.map(|_| [255; 4]),
        artboards: false,
        anti_alias: AntiAlias::None,
        overprint_preview: true,
        proof: plate.map(|p| ProofSetup { separations: Some(vec![p.into()]), ..Default::default() }),
        ..Default::default()
    }
}

fn outline(mask: &[bool], w: usize, h: usize, region: Rect) -> Result<PathData> {
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
    log::debug!("trap: cleaning {} anchors", path.subpaths.iter().map(|p| p.anchors.len()).sum::<usize>());
    let spread = vectorcraft_pathops::try_offset_path(&path, 0.01 * MM, vectorcraft_pathops::Join::Miter, 4.0)
        .map_err(|e| bad(C, format!("clip spread: {e}")))?;
    vectorcraft_pathops::try_offset_path(&spread, -0.01 * MM, vectorcraft_pathops::Join::Miter, 4.0).map_err(|e| bad(C, format!("clip choke: {e}")))
}

fn image_node(d: &mut Document, key: String, w: u32, h: u32, xf: Affine) -> Node {
    Node::new(d.alloc_id(), NodeKind::Image(Box::new(ImageObject { key, width: w, height: h, xf, link: None, placement: Default::default() })))
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
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
    source.drop_lookup_cache();
    let bounds = source.art_bounds().ok_or_else(|| bad(C, "Artwork has no bounds"))?.inflate(0.3 * MM, 0.3 * MM);
    let region = Rect::new(
        (bounds.x0 * SCALE).floor() / SCALE,
        (bounds.y0 * SCALE).floor() / SCALE,
        (bounds.x1 * SCALE).ceil() / SCALE,
        (bounds.y1 * SCALE).ceil() / SCALE,
    );
    let (w, h) = vectorcraft_render::raster_size(region, SCALE).map_err(|e| bad(C, e))?;
    let n = w as usize * h as usize;
    if n > MAX_PIXELS {
        return Err(bad(C, format!("prototype grid {w} x {h} exceeds 64 million pixels at 600 dpi")));
    }
    let mut eligible = source.clone();
    for layer in &mut eligible.layers {
        eligibility(Arc::make_mut(layer), &source, threshold as f32)?;
    }
    let mut renderer = Renderer::new();
    log::debug!("trap: rendering {w} x {h}");
    let rgba = renderer.render_region_with(&source, region, SCALE, &options(None));
    let eligible_rgba = renderer.render_region_with(&eligible, region, SCALE, &options(None));
    crate::cancel::check()?;
    if rgba.pixels.len() != n * 4 || eligible_rgba.pixels.len() != n * 4 {
        return Err(bad(C, "incomplete artwork render"));
    }
    let occupied: Vec<_> = rgba.pixels.as_chunks::<4>().0.iter().map(|[_, _, _, alpha]| *alpha >= 128).collect();
    let allowed: Vec<_> = eligible_rgba.pixels.as_chunks::<4>().0.iter().map(|[red, _, _, _]| *red >= 128).collect();
    drop(rgba);
    drop(eligible_rgba);
    log::debug!("trap: vector union");
    let union = if let Some(path) = union::artwork(&source)? {
        let mut silhouette = source.clone();
        let shape = Node::path(silhouette.alloc_id(), path, Appearance::basic(Paint::solid(Color::BLACK), Paint::None, 0.0));
        silhouette.layers = vec![Arc::new(shape)];
        silhouette.drop_lookup_cache();
        let rgba = renderer.render_region_with(&silhouette, region, SCALE, &options(None));
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
    let nearest = pixels::band(&occupied, &allowed, &union, w as usize, h as usize, 0.2 * 600.0 / 25.4)?;
    let mask: Vec<_> = nearest.iter().map(Option::is_some).collect();
    let count = mask.iter().filter(|&&v| v).count();
    let existing: Vec<_> = doc.layers.iter().filter(|l| l.name.as_deref() == Some("Auto Trap-1")).map(|l| l.id).collect();
    crate::cancel::check()?;
    if count == 0 {
        s.edit("Trap", |d, _| {
            d.remove_many(&existing)?;
            Ok(())
        })?;
        return Ok(json!({"pixels":0,"width":w,"height":h,"blackThreshold":threshold,"seconds":elapsed()}));
    }
    log::debug!("trap: tracing {count} pixels");
    let clip = outline(&mask, w as usize, h as usize, region)?;
    if clip.is_empty() {
        return Err(bad(C, "clip cleanup unexpectedly removed the band"));
    }
    log::debug!("trap: plate samples");
    let plates = vectorcraft_doc::inks::plates(&source);
    if plates.len() > 32 {
        return Err(bad(C, "too many inks for DeviceN"));
    }
    let mut process = vec![0; n * 4];
    let spot_count = plates.len().saturating_sub(4);
    let mut spot = vec![0; n * spot_count];
    for (channel, plate) in plates.iter().enumerate() {
        crate::cancel::check()?;
        let rendered = renderer.render_region_with(&source, region, SCALE, &options(Some(&plate.name)));
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
        let clip = Node::path(d.alloc_id(), clip, Appearance::basic(Paint::solid(Color::BLACK), Paint::None, 0.0));
        let mut group = Node::group(d.alloc_id(), vec![Arc::new(clip), Arc::new(image)]);
        if let NodeKind::Group { clip, .. } = &mut group.kind {
            *clip = true;
        }
        let mut layer = Node::layer(d.alloc_id(), "Auto Trap-1", LayerColor::Preset(0));
        if let Some(children) = layer.children_mut() {
            children.push(Arc::new(group));
        }
        d.insert(None, 0, layer)?;
        Ok(())
    })?;
    Ok(json!({"pixels":count,"width":w,"height":h,"blackThreshold":threshold,"seconds":elapsed()}))
}
