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
