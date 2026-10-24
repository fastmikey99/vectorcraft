//! Pixel-aligned horizontal bands, with a halo for the finite trap/closing kernels.
use super::*;
pub(super) struct Band {
    pub region: Rect,
    pub height: u32,
    pub first: usize,
    pub last: usize,
}
pub(super) fn rows(w: u32, h: u32, region: Rect, halo: u32) -> impl Iterator<Item = Band> {
    let rows = (BAND_PIXELS / (w.max(1) as usize)).max(1) as u32;
    (0..h).step_by(rows as usize).map(move |y| {
        let start = y.saturating_sub(halo);
        let end = (y + rows).min(h);
        let bottom = end.saturating_add(halo).min(h);
        Band {
            region: Rect::new(region.x0, region.y0 + f64::from(start) / SCALE, region.x1, region.y0 + f64::from(bottom) / SCALE),
            height: bottom - start,
            first: (y - start) as usize,
            last: (end - start) as usize,
        }
    })
}
