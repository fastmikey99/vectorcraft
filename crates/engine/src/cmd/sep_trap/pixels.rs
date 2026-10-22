//! Bounded exact nearest-feature transform: only distances inside the fixed trap radius matter.
//! Occupancy comes from the vector union (or a closed renderer mask). Colour samples always
//! come from the original art, and neither representation can receive ink-to-ink bands.
use super::{C, Result, bad};

/// Separable 3×3 dilation followed by erosion. Only occupancy is closed: newly filled seam
/// pixels never become colour samples. Keep original edge pixels when the grid clips the kernel.
pub(super) fn close_union(occupied: &[bool], w: usize, h: usize) -> Vec<bool> {
    let axis = |input: &[bool], vertical: bool, dilate: bool| -> Vec<bool> {
        let (step, limit) = if vertical { (w, h) } else { (1, w) };
        input
            .iter()
            .enumerate()
            .map(|(i, &center)| {
                let coordinate = if vertical { i / w } else { i % w };
                let before = coordinate > 0 && input.get(i - step) == Some(&true);
                let after = coordinate + 1 < limit && input.get(i + step) == Some(&true);
                if dilate { before || center || after } else { before && center && after }
            })
            .collect()
    };
    let horizontal = axis(occupied, false, true);
    let dilated = axis(&horizontal, true, true);
    drop(horizontal);
    let horizontal = axis(&dilated, false, false);
    drop(dilated);
    let mut closed = axis(&horizontal, true, false);
    for (pixel, &original) in closed.iter_mut().zip(occupied) {
        *pixel |= original;
    }
    closed
}

pub(super) fn band(occupied: &[bool], eligible: &[bool], union: &[bool], w: usize, h: usize, radius: f64) -> Result<Vec<Option<u32>>> {
    let n = w.checked_mul(h).filter(|&n| n <= super::MAX_PIXELS && n > 0).ok_or_else(|| bad(C, "invalid trap grid"))?;
    if occupied.len() != n || eligible.len() != n || union.len() != n || !(0.0..=8.0).contains(&radius) {
        return Err(bad(C, "invalid trap samples"));
    }
    let outside: Vec<_> = union.iter().zip(occupied).map(|(&united, &sampled)| !united && !sampled).collect();
    let r = radius.ceil() as i32;
    let mut offsets = Vec::new();
    for dy in -r..=r {
        for dx in -r..=r {
            let distance = dx * dx + dy * dy;
            // Occupied samples represent unit pixel cells, not dimensionless points.
            // Test the destination centre against the nearest point of the source cell;
            // centre-to-centre distance would shorten the outward band by half a pixel.
            let edge_x = (f64::from(dx.abs()) - 0.5).max(0.0);
            let edge_y = (f64::from(dy.abs()) - 0.5).max(0.0);
            if edge_x * edge_x + edge_y * edge_y <= radius * radius {
                offsets.push((distance, dx, dy));
            }
        }
    }
    let mut result = vec![None; n];
    let mut distance = vec![u8::MAX; n];
    for i in 0..n {
        if occupied.get(i) != Some(&true) {
            continue;
        }
        let (x, y) = ((i % w) as i32, (i / w) as i32);
        let edge = [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().any(|&(dx, dy)| {
            let (sx, sy) = (x + dx, y + dy);
            sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 && occupied.get(sy as usize * w + sx as usize) == Some(&false)
        });
        if !edge {
            continue;
        }
        for &(squared, dx, dy) in &offsets {
            let (sx, sy) = (x + dx, y + dy);
            if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                continue;
            }
            let j = sy as usize * w + sx as usize;
            if outside.get(j) != Some(&true) {
                continue;
            }
            if let Some(d) = distance.get_mut(j)
                && squared < i32::from(*d)
            {
                *d = squared as u8;
                if let Some(out) = result.get_mut(j) {
                    *out = Some(i as u32);
                }
            }
        }
    }
    // Source order breaks exact distance ties deterministically. Exclusions apply after nearest
    // ownership is decided, so an excluded edge cannot be replaced by a more distant colour.
    for out in &mut result {
        if out.is_some_and(|i| eligible.get(i as usize) != Some(&true)) {
            *out = None;
        }
    }
    Ok(result)
}
