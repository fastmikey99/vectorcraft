//! PAL Rasterize: original ink planes, transparent background, and an editable clip.
use super::*;
use vectorcraft_doc::NodeId;

pub(crate) fn rasterize(s: &mut Session, ids: &[NodeId]) -> Result<usize> {
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
        return Err(bad(C, "Rasterize exceeds 64 million pixels"));
    }
    let plates = vectorcraft_doc::inks::plates(&source);
    if plates.len() > 32 {
        return Err(bad(C, "too many inks for DeviceN"));
    }
    let mut renderer = Renderer::new();
    let rgba = renderer.render_region_with(&source, region, SCALE, &options(None));
    if rgba.pixels.len() != n * 4 {
        return Err(bad(C, "incomplete artwork render"));
    }
    let alpha: Vec<_> = rgba.pixels.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
    let mask: Vec<_> = alpha.iter().map(|a| *a != 0).collect();
    let clip = outline(&mask, w as usize, h as usize, region, false)?;
    let mut process = vec![0; n * 4];
    let per = plates.len().saturating_sub(4);
    let mut spot = vec![0; n * per];
    for (channel, plate) in plates.iter().enumerate() {
        crate::cancel::check()?;
        let rgba = renderer.render_region_with(&source, region, SCALE, &options(Some(&plate.name)));
        if rgba.pixels.len() != n * 4 {
            return Err(bad(C, "incomplete plate render"));
        }
        for (i, (pixel, alpha)) in rgba.pixels.as_chunks::<4>().0.iter().zip(&alpha).enumerate() {
            // The white plate background composites coverage with alpha. Store unassociated
            // ink samples because the image's opacity mask applies alpha at draw time.
            let ink = if *alpha == 0 { 0 } else { ((255 - u32::from(pixel[0])) * 255 / u32::from(*alpha)).min(255) as u8 };
            let target = if channel < 4 { process.get_mut(i * 4 + channel) } else { spot.get_mut(i * per + channel - 4) };
            if let Some(v) = target {
                *v = ink;
            }
        }
    }
    let inks = vectorcraft_doc::cmyk::Inks::new(w, h, process).ok_or_else(|| bad(C, "invalid process samples"))?;
    let blob = if per == 0 {
        ImageBlob::cmyk_tiff(&inks)
    } else {
        let spots = vectorcraft_doc::cmyk::SpotInks::new(w, h, plates.into_iter().skip(4).map(|p| p.name).collect(), spot)
            .ok_or_else(|| bad(C, "invalid spot samples"))?;
        ImageBlob::cmyk_spot_tiff(&inks, &spots)
    }
    .ok_or_else(|| bad(C, "could not encode ink image"))?;
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
