//! PAL Rasterize: original ink planes, transparent background, and an editable clip.
use super::*;
use vectorcraft_doc::NodeId;

pub(crate) fn rasterize(s: &mut Session, ids: &[NodeId]) -> Result<usize> {
    rasterize_with_limit(s, ids, 64_000_000)
}

pub(super) fn rasterize_with_limit(s: &mut Session, ids: &[NodeId], full_grid_limit: usize) -> Result<usize> {
    rasterize_with_budgets(s, ids, full_grid_limit, MAX_CONTOUR_ANCHORS)
}

pub(super) fn rasterize_with_budgets(s: &mut Session, ids: &[NodeId], full_grid_limit: usize, contour_limit: usize) -> Result<usize> {
    let doc = &s.doc()?.doc;
    let mut source = (**doc).clone();
    source.layers = ids.iter().filter_map(|id| doc.node(*id).cloned().map(Arc::new)).collect();
    source.drop_lookup_cache();
    let b = source.art_bounds().ok_or_else(|| bad(C, "Rasterize selection has no bounds"))?;
    let region =
        Rect::new((b.x0 * SCALE).floor() / SCALE, (b.y0 * SCALE).floor() / SCALE, (b.x1 * SCALE).ceil() / SCALE, (b.y1 * SCALE).ceil() / SCALE);
    let (w, h) = vectorcraft_render::raster_size(region, SCALE).map_err(|e| bad(C, e))?;
    let n = w as usize * h as usize;
    if n > MAX_PIXELS {
        return Err(bad(C, "Rasterize exceeds the 150-million-pixel limit (A2 at 600 dpi)"));
    }
    let plates = if n > full_grid_limit { used_plates(&source) } else { vectorcraft_doc::inks::plates(&source) };
    if plates.len() > 32 {
        return Err(bad(C, "too many inks for DeviceN"));
    }
    // One image preserves interpolation across band boundaries in the production RIP.
    // Only rendering scratch is banded; the final ink/mask planes are bounded explicitly.
    let per = plates.len().saturating_sub(4);
    if n as u64 > vectorcraft_doc::cmyk::MAX_CMYK_PIXELS {
        return Err(bad(C, "Rasterize exceeds the decoded image limit; use the native builder for large transfers"));
    }
    if (n as u64).saturating_mul(plates.len() as u64) > vectorcraft_doc::cmyk::spots::MAX_INK_BYTES {
        return Err(bad(C, "Rasterize exceeds the decoded ink memory limit at 600 dpi; reduce the transfer size or ink count"));
    }
    let mut alpha = vec![0; n];
    let mut process = vec![0; n * 4];
    let mut spot = vec![0; n * per];
    let rows = if n <= full_grid_limit { h } else { (BAND_PIXELS / w as usize).max(1) as u32 };
    let bands = || {
        (0..h).step_by(rows as usize).map(|y| {
            let bottom = (y + rows).min(h);
            let band = Rect::new(region.x0, region.y0 + f64::from(y) / SCALE, region.x1, region.y0 + f64::from(bottom) / SCALE);
            (band, y as usize * w as usize, (bottom - y) as usize * w as usize)
        })
    };
    {
        let mut renderer = Renderer::new();
        for (band, start, len) in bands() {
            crate::cancel::check()?;
            let decoded = vectorcraft_render::decode_failures();
            let rgba = renderer.render_region_with(&source, band, SCALE, &options(None));
            if let Some(reason) = vectorcraft_render::decode_failure_reason(decoded) {
                return Err(bad(C, reason));
            }
            if rgba.pixels.len() != len * 4 {
                return Err(bad(C, "incomplete artwork render"));
            }
            for (i, pixel) in rgba.pixels.as_chunks::<4>().0.iter().enumerate() {
                if let Some(a) = alpha.get_mut(start + i) {
                    *a = pixel[3];
                }
            }
        }
    }
    // Keep only one plate's decoded proof-image cache, not a full-page RGBA cache
    // for display plus every ink. Iterate all bands of a plate before dropping it.
    for (channel, plate) in plates.iter().enumerate() {
        let mut renderer = Renderer::new();
        for (band, start, len) in bands() {
            crate::cancel::check()?;
            let decoded = vectorcraft_render::decode_failures();
            let rgba = renderer.render_region_with(&source, band, SCALE, &options(Some(&plate.name)));
            if let Some(reason) = vectorcraft_render::decode_failure_reason(decoded) {
                return Err(bad(C, reason));
            }
            if rgba.pixels.len() != len * 4 {
                return Err(bad(C, "incomplete plate render"));
            }
            for (i, pixel) in rgba.pixels.as_chunks::<4>().0.iter().enumerate() {
                let at = start + i;
                let a = alpha.get(at).copied().unwrap_or(0);
                let ink = if a == 0 { 0 } else { ((255 - u32::from(pixel[0])) * 255 / u32::from(a)).min(255) as u8 };
                let target = if channel < 4 { process.get_mut(at * 4 + channel) } else { spot.get_mut(at * per + channel - 4) };
                if let Some(v) = target {
                    *v = ink;
                }
            }
        }
    }
    let mask: Vec<_> = alpha.iter().map(|a| *a != 0).collect();
    let clip = outline_with_budget(&mask, w as usize, h as usize, region, false, contour_limit)?;
    drop(mask);
    let inks = vectorcraft_doc::cmyk::Inks::new(w, h, process).ok_or_else(|| bad(C, "invalid process samples"))?;
    let blob = if per == 0 {
        ImageBlob::cmyk_tiff(&inks)
    } else {
        let spots = vectorcraft_doc::cmyk::SpotInks::new(w, h, plates.into_iter().skip(4).map(|p| p.name).collect(), spot)
            .ok_or_else(|| bad(C, "invalid spot samples"))?;
        ImageBlob::cmyk_spot_tiff(&inks, &spots)
    }
    .ok_or_else(|| bad(C, "could not encode ink image"))?;
    drop(inks);
    let mut png = std::io::Cursor::new(Vec::new());
    image::GrayImage::from_raw(w, h, alpha)
        .ok_or_else(|| bad(C, "invalid mask"))?
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| bad(C, e.to_string()))?;
    let xf = Affine::translate((region.x0, region.y0)) * Affine::scale(1.0 / SCALE);
    s.edit("Trap", |d, sel| {
        let key = super::super::menucmds::unique_key(d, "raster-inks");
        d.images.insert(key.clone(), blob);
        let mut image = image_node(d, key, w, h, xf);
        let key = super::super::menucmds::unique_key(d, "raster-alpha");
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
        let id = group.id;
        d.wrap(ids, |_| group)?;
        sel.set([id]);
        Ok(1)
    })
}

// Large generated traps must not allocate full-page channels for unused template
// swatches. Only prune when a complete flat-paint/image walk proves absence;
// registration and complex appearances conservatively retain every ink.
fn used_plates(doc: &Document) -> Vec<vectorcraft_doc::inks::Plate> {
    use std::collections::BTreeSet;
    fn collect(doc: &Document, n: &Node, names: &mut BTreeSet<String>) -> bool {
        if !n.appearance.effects.is_empty() {
            return false;
        }
        for item in &n.appearance.items {
            let paint = match item {
                vectorcraft_doc::AppearanceItem::Fill(f) => &f.paint,
                vectorcraft_doc::AppearanceItem::Stroke(s) => &s.paint,
            };
            match paint {
                Paint::None => (),
                Paint::Solid { swatch, .. } => {
                    if let Some(name) = swatch {
                        if name == vectorcraft_color::swatch::REGISTRATION {
                            return false;
                        }
                        names.insert(name.clone());
                    }
                }
                _ => return false,
            }
        }
        match &n.kind {
            NodeKind::Image(im) => {
                let Some(blob) = doc.images.get(&im.key) else {
                    return false;
                };
                names.extend(blob.spot_names());
            }
            NodeKind::Path { .. } | NodeKind::Compound { .. } | NodeKind::Group { .. } | NodeKind::Layer { .. } => (),
            _ => return false,
        }
        if let Some(mask) = &n.extras().mask
            && !collect(doc, &mask.art, names)
        {
            return false;
        }
        n.children().is_none_or(|children| children.iter().all(|c| collect(doc, c, names)))
    }
    let mut names = BTreeSet::new();
    let complete = doc.layers.iter().all(|n| collect(doc, n, &mut names));
    vectorcraft_doc::inks::plates(doc).into_iter().filter(|p| !complete || !p.spot || names.contains(&p.name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_raster_only_prunes_provably_unused_spots() {
        let mut d = Document::new(10., 10.);
        for name in ["Used", "Unused"] {
            d.swatches.push(vectorcraft_color::Swatch { name: name.into(), paint: Paint::solid(Color::BLACK), global: true, spot: true });
        }
        let mut n = Node::path(
            d.alloc_id(),
            vectorcraft_geom::shapes::rectangle(Rect::new(0., 0., 10., 10.)),
            Appearance::basic(Paint::Solid { color: Color::BLACK, swatch: Some("Used".into()), tint: 1. }, Paint::None, 0.),
        );
        d.layers = vec![Arc::new(n.clone())];
        assert_eq!(used_plates(&d).iter().filter(|p| p.spot).map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Used"]);
        n.appearance = Appearance::basic(
            Paint::Solid { color: Color::BLACK, swatch: Some(vectorcraft_color::swatch::REGISTRATION.into()), tint: 1. },
            Paint::None,
            0.,
        )
        .into();
        d.layers = vec![Arc::new(n)];
        assert_eq!(used_plates(&d).len(), 6);
    }
}
