//! Flat ink ownership at the nearest vector boundary. Raster cells choose the band extent;
//! the actual outline chooses its paint, avoiding colour switches caused by pixel-centre ties.
use super::*;
use vectorcraft_color::BlendMode;
use vectorcraft_doc::AppearanceItem;
use vectorcraft_geom::{FillRule, Point};

struct Feature {
    path: PathData,
    rule: FillRule,
    bounds: Rect,
    ink: vectorcraft_doc::inks::Inks,
    eligible: bool,
}

pub(super) struct Samples {
    features: Vec<Feature>,
    pixels: Vec<(usize, usize)>,
}

fn collect(node: &Node, doc: &Document, threshold: f32, inherited: bool, out: &mut Vec<Feature>) -> Result<bool> {
    crate::cancel::check()?;
    // Coincident no-bleed and artwork outlines have different paints. Their visible
    // owner is determined by stacking, not nearest-curve ties. The renderer retains
    // that ownership (including clips) and its white eligibility prevents a band.
    if matches!(node.name.as_deref(), Some("LIGHT NO BLEED" | "NO WHP NO BLEED" | "NO POWDER NO BLEED" | "NO WHP OR POWDER NO BLEED")) {
        return Ok(false);
    }
    let permission = node.extras().trap_external.unwrap_or(inherited);
    if !node.visible || node.opacity <= 0.0 || matches!(node.kind, NodeKind::Path { guide: true, .. }) {
        return Ok(true);
    }
    if node.opacity != 1.0
        || node.blend != BlendMode::Normal
        || node.knockout != Default::default()
        || has_render_extras(node)
        || !node.appearance.effects.is_empty()
    {
        return Ok(false);
    }
    match &node.kind {
        NodeKind::Group { children, clip } | NodeKind::Layer { children, clip, .. } => {
            if *clip || !node.appearance.items.is_empty() {
                return Ok(false);
            }
            for child in children {
                if !collect(child, doc, threshold, permission, out)? {
                    return Ok(false);
                }
            }
        }
        NodeKind::Path { .. } | NodeKind::Compound { .. } => {
            let mut fill = None;
            for item in &node.appearance.items {
                match item {
                    AppearanceItem::Fill(f) if f.visible && !f.paint.is_none() && f.opacity > 0.0 => {
                        if fill.is_some() || f.opacity != 1.0 || f.blend != BlendMode::Normal || f.overprint || !f.effects.is_empty() {
                            return Ok(false);
                        }
                        fill = Some(f);
                    }
                    AppearanceItem::Stroke(s) if s.visible && !s.paint.is_none() && s.width > 0.0 && s.opacity > 0.0 => return Ok(false),
                    _ => {}
                }
            }
            if let Some(fill) = fill {
                let Paint::Solid { color, swatch, tint } = &fill.paint else {
                    return Ok(false);
                };
                let (mut path, rule) = super::super::pathops::node_path(node).ok_or_else(|| bad(C, "missing colour outline"))?;
                // Filled open subpaths have an implicit closing edge (also used by the renderer).
                for subpath in &mut path.subpaths {
                    subpath.closed = true;
                }
                if let Some(bounds) = path.bounds() {
                    let (ink, eligible) = paint_ink(doc, color, swatch.as_deref(), *tint, threshold);
                    // Include the source/destination cell uncertainty of the raster band.
                    let padding = 0.2 * MM + 2.0 / SCALE;
                    out.push(Feature { path, rule, bounds: bounds.inflate(padding, padding), ink, eligible: eligible && permission });
                }
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Complex appearances retain the existing renderer sampling. A flat opaque object has one
/// unambiguous ink value, so only its geometric boundary is needed to choose that value.
pub(super) fn samples(doc: &Document, nearest: &mut [Option<u32>], w: usize, region: Rect, threshold: f32) -> Result<Option<Samples>> {
    if nearest.iter().all(Option::is_none) {
        return Ok(None);
    }
    let mut features = Vec::new();
    for layer in &doc.layers {
        if !collect(layer, doc, threshold, true, &mut features)? {
            return Ok(None);
        }
    }
    // Remove paint hidden by later opaque fills. A covered outline has no colour
    // ownership, even when its distance ties the front outline exactly.
    let mut visibility_checks = 0usize;
    for back in 0..features.len() {
        crate::cancel::check()?;
        let mut visible = features.get(back).map(|f| f.path.clone()).unwrap_or_default();
        let mut rule = features.get(back).map_or(FillRule::NonZero, |f| f.rule);
        for front in features.iter().skip(back + 1) {
            crate::cancel::check()?;
            visibility_checks += 1;
            // Very large overlap stacks retain renderer sampling. Geometry ownership
            // is optional; it must not turn a bounded raster task into quadratic work.
            if visibility_checks > 1_000_000 {
                return Ok(None);
            }
            if visible.bounds().zip(front.path.bounds()).is_some_and(|(a, b)| a.intersect(b).area() > 0.0) {
                visible = match vectorcraft_pathops::try_boolean(
                    &visible,
                    rule,
                    &front.path,
                    front.rule,
                    vectorcraft_pathops::BoolOp::Difference,
                    vectorcraft_pathops::DEFAULT_PRECISION,
                ) {
                    Ok(path) => {
                        rule = FillRule::NonZero;
                        path
                    }
                    // Degenerate intersections cannot establish ownership; the renderer can.
                    Err(_) => return Ok(None),
                };
            }
        }
        if let Some(feature) = features.get_mut(back) {
            feature.path = visible;
        }
    }
    if w == 0 {
        return Err(bad(C, "invalid colour sample grid"));
    }
    let index = super::index::Index::new(features.iter().enumerate().rev().map(|(i, f)| (i, f.path.clone())));
    let mut pixels = Vec::new();
    for (i, sample) in nearest.iter_mut().enumerate() {
        if i % 16384 == 0 {
            crate::cancel::check()?;
        }
        if sample.is_none() {
            continue;
        }
        let point = Point::new(region.x0 + ((i % w) as f64 + 0.5) / SCALE, region.y0 + ((i / w) as f64 + 0.5) / SCALE);
        let j = index
            .nearest(point, |j| features.get(j).is_some_and(|f| f.bounds.contains(point)))
            .ok_or_else(|| bad(C, "no vector colour owner for band sample"))?;
        let feature = features.get(j).ok_or_else(|| bad(C, "invalid vector colour owner"))?;
        // The same PAL epsilon/K rule applies after geometric ownership, just as it does after
        // raster ownership. An excluded nearer object cannot borrow a more distant object's ink.
        if feature.eligible {
            pixels.push((i, j));
        } else {
            *sample = None;
        }
    }
    Ok(Some(Samples { features, pixels }))
}

impl Samples {
    pub(super) fn write(&self, plates: &[vectorcraft_doc::inks::Plate], process: &mut [u8], spots: &mut [u8]) {
        let per = plates.len().saturating_sub(4);
        for &(pixel, owner) in &self.pixels {
            if let Some(feature) = self.features.get(owner) {
                for (channel, plate) in plates.iter().enumerate() {
                    let ink = (feature.ink.on_plate(&plate.name).clamp(0.0, 1.0) * 255.0).round() as u8;
                    let target = if channel < 4 { process.get_mut(pixel * 4 + channel) } else { spots.get_mut(pixel * per + channel - 4) };
                    if let Some(target) = target {
                        *target = ink;
                    }
                }
            }
        }
    }
}
