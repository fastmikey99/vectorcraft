//! Pre-Rasterize TrapTask form: vector runs in the original process and spot inks.
//! Runs lie on the established 600 dpi trap grid; no clip spread/choke is applied here.
use super::*;
use std::collections::BTreeMap;
use vectorcraft_doc::{AppearanceItem, FillLayer, NodeId};

pub(super) struct Band<'a> {
    pub mask: &'a [bool],
    pub process: &'a [u8],
    pub spot: &'a [u8],
    pub plates: &'a [vectorcraft_doc::inks::Plate],
    pub width: usize,
    pub region: Rect,
}
pub(super) fn insert(s: &mut Session, band: Band<'_>, clip: PathData, existing: &[NodeId]) -> Result<()> {
    let per = band.plates.len().saturating_sub(4);
    let mut paths: BTreeMap<Vec<u8>, BezPath> = BTreeMap::new();
    let colour = |i: usize| -> Vec<u8> {
        band.process
            .get(i * 4..i * 4 + 4)
            .unwrap_or_default()
            .iter()
            .chain(band.spot.get(i * per..i * per + per).unwrap_or_default())
            .copied()
            .collect()
    };
    let mut runs = 0usize;
    for (y, row) in band.mask.chunks(band.width).enumerate() {
        crate::cancel::check()?;
        let mut x = 0;
        while x < row.len() {
            if !row.get(x).copied().unwrap_or(false) {
                x += 1;
                continue;
            }
            let start = x;
            let ink = colour(y * band.width + x);
            x += 1;
            while x < row.len() && row.get(x).copied().unwrap_or(false) && colour(y * band.width + x) == ink {
                x += 1;
            }
            runs += 1;
            if runs > 1_000_000 {
                return Err(bad(C, "trap vector band exceeds one million runs"));
            }
            let path = paths.entry(ink).or_default();
            let (x0, x1) = (band.region.x0 + start as f64 / SCALE, band.region.x0 + x as f64 / SCALE);
            let (y0, y1) = (band.region.y0 + y as f64 / SCALE, band.region.y0 + (y + 1) as f64 / SCALE);
            path.move_to((x0, y0));
            path.line_to((x1, y0));
            path.line_to((x1, y1));
            path.line_to((x0, y1));
            path.close_path();
        }
    }
    s.edit("Trap", |d, _| {
        d.remove_many(existing)?;
        let mut mask = Node::path(d.alloc_id(), clip, Appearance::basic(Paint::None, Paint::None, 0.0));
        if let NodeKind::Path { clipping, .. } = &mut mask.kind {
            *clipping = true;
        }
        let mut children = vec![Arc::new(mask)];
        for (ink, path) in paths {
            let mut appearance = Appearance::default();
            appearance.items.clear();
            if let Some(&[c, m, y, k]) = ink.get(..4).and_then(|v| <&[u8; 4]>::try_from(v).ok())
                && [c, m, y, k].iter().any(|v| *v != 0)
            {
                let mut fill = FillLayer::new(Paint::None);
                fill.paint = Paint::solid(Color::cmyk(f32::from(c) / 255., f32::from(m) / 255., f32::from(y) / 255., f32::from(k) / 255.));
                appearance.items.push(AppearanceItem::Fill(fill));
            }
            for (plate, tint) in band.plates.iter().skip(4).zip(ink.iter().skip(4)) {
                if *tint == 0 {
                    continue;
                }
                let mut fill = FillLayer::new(Paint::None);
                fill.paint = Paint::Solid {
                    color: vectorcraft_doc::inks::spot_color(d, &plate.name).unwrap_or(Color::BLACK),
                    swatch: Some(plate.name.clone()),
                    tint: f32::from(*tint) / 255.,
                };
                fill.overprint = !appearance.items.is_empty();
                appearance.items.push(AppearanceItem::Fill(fill));
            }
            children.push(Arc::new(Node::path(d.alloc_id(), PathData::from_bezpath(&path), appearance)));
        }
        let mut group = Node::group(d.alloc_id(), children);
        if let NodeKind::Group { clip, .. } = &mut group.kind {
            *clip = true;
        }
        let mut layer = Node::layer(d.alloc_id(), "Trap Layer 1", LayerColor::Preset(0));
        if let Some(cs) = layer.children_mut() {
            cs.push(Arc::new(group));
        }
        d.insert(None, usize::MAX, layer)?;
        Ok(())
    })
}
