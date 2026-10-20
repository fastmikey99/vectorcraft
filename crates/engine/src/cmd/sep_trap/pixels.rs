//! Bounded exact nearest-feature transform: only distances inside the fixed trap radius matter.
//! Every unpainted pixel is outside the art union, including blank counters. Ink-to-ink edges
//! remain inside the union and never receive a band.
use super::{C, Result, bad};

pub(super) fn band(occupied: &[bool], eligible: &[bool], w: usize, h: usize, radius: f64) -> Result<Vec<Option<u32>>> {
    let n = w.checked_mul(h).filter(|&n| n <= super::MAX_PIXELS && n > 0).ok_or_else(|| bad(C, "invalid trap grid"))?;
    if occupied.len() != n || eligible.len() != n || !(0.0..=8.0).contains(&radius) {
        return Err(bad(C, "invalid trap samples"));
    }
    let outside: Vec<_> = occupied.iter().map(|&on| !on).collect();
    let r = radius.ceil() as i32;
    let mut offsets = Vec::new();
    for dy in -r..=r {
        for dx in -r..=r {
            let distance = dx * dx + dy * dy;
            if f64::from(distance) <= radius * radius {
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
            sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 && outside.get(sy as usize * w + sx as usize) == Some(&true)
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
