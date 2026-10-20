//! Exact binary masks are preparation artifacts, never tone-derived occupancy guesses.
use serde::{Deserialize, Serialize};
use vectorcraft_geom::{Affine, BezPath, FillRule, PathData, Rect, Shape};

const MAX_PIXELS: usize = 1_048_576;
const MAX_RUNS: usize = 16_384;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    pub source_sha256: String,
    pub width: u32,
    pub height: u32,
    pub page: usize,
    /// Top-down pixel-edge coordinates to page-local millimetres.
    pub pixel_to_page_mm: [f64; 6],
    /// Row-major bits, high bit first, zero padding in the last byte.
    pub bits_hex: String,
    /// Optional exact rational pixel-edge crop, separately reviewed from alpha.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<crate::prepared_crop::Clip>,
}
impl Mask {
    fn bytes(&self) -> Result<Vec<u8>, String> {
        let pixels = (self.width as usize).checked_mul(self.height as usize).ok_or("Prepared mask dimensions overflow")?;
        if self.width == 0 || self.height == 0 || pixels > MAX_PIXELS || self.page >= 1000 {
            return Err("Prepared mask dimensions/page exceed budget".into());
        }
        if self.source_sha256.len() != 64 || !self.source_sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("Prepared mask source identity is invalid".into());
        }
        let len = pixels.div_ceil(8);
        if self.bits_hex.len() != len * 2 {
            return Err("Prepared mask packed length does not match dimensions".into());
        }
        let mut bytes = Vec::with_capacity(len);
        let nibble = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        for [a, b] in self.bits_hex.as_bytes().as_chunks::<2>().0 {
            bytes.push((nibble(*a).ok_or("Invalid mask hex digit")? << 4) | nibble(*b).ok_or("Invalid mask hex digit")?);
        }
        if pixels % 8 != 0 && bytes.last().is_some_and(|b| b & ((1 << (8 - pixels % 8)) - 1) != 0) {
            return Err("Prepared mask padding bits must be zero".into());
        }
        let xf = Affine::new(self.pixel_to_page_mm);
        if self.pixel_to_page_mm.iter().any(|n| !n.is_finite() || n.abs() > 1_000_000.0) || !xf.determinant().is_finite() || xf.determinant() == 0.0 {
            return Err("Prepared mask transform is invalid".into());
        }
        let bounds = xf.transform_rect_bbox(Rect::new(0.0, 0.0, f64::from(self.width), f64::from(self.height)));
        if [bounds.x0, bounds.y0, bounds.x1, bounds.y1].iter().any(|n| !n.is_finite() || n.abs() > 1_000_000.0) {
            return Err("Prepared mask placement exceeds coordinate bounds".into());
        }
        if let Some(clip) = &self.clip {
            clip.validate(self.width, self.height)?;
        }
        Ok(bytes)
    }
    pub(crate) fn cell_visible(&self, x: u32, y: u32) -> bool {
        self.clip.as_ref().is_none_or(|clip| clip.intersects(x, y))
    }
    pub(crate) fn boundary_policy(&self) -> &'static str {
        if self.clip.is_some() { "exact_binary_alpha_with_retained_crop_v1" } else { "exact_binary_alpha" }
    }
    pub fn validate(&self) -> Result<(), String> {
        self.bytes().map(|_| ())
    }
    /// Exact non-transparent pixel rectangle, excluding encoding-only padding.
    pub fn visible_window(&self) -> Result<Option<[u32; 4]>, String> {
        let bytes = self.bytes()?;
        let (mut x0, mut y0, mut x1, mut y1) = (self.width, self.height, 0, 0);
        for y in 0..self.height {
            for x in 0..self.width {
                let at = y as usize * self.width as usize + x as usize;
                if self.cell_visible(x, y) && bytes.get(at / 8).is_some_and(|b| b & (1 << (7 - at % 8)) != 0) {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        Ok((x1 > x0 && y1 > y0).then(|| [x0, y0, x1 - x0, y1 - y0]))
    }
    /// Pixel-space boolean uncertainty scaled conservatively into physical space.
    pub fn geometry_error_mm(&self) -> f64 {
        let [a, b, c, d, _, _] = self.pixel_to_page_mm;
        let norm = ((a.abs() + b.abs()).max(c.abs() + d.abs()) * (a.abs() + c.abs()).max(b.abs() + d.abs())).sqrt();
        0.00001 * norm + 0.000000001
    }
    pub fn path(&self) -> Result<PathData, String> {
        use std::collections::BTreeMap;
        let bytes = self.bytes()?;
        let width = self.width as usize;
        let occupied = |x: usize, y: usize| {
            let index = y * width + x;
            bytes.get(index / 8).is_some_and(|b| b & (1 << (7 - index % 8)) != 0)
        };
        let mut active: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        let mut rectangles = Vec::new();
        let mut runs = 0usize;
        for y in 0..=self.height as usize {
            let mut next = BTreeMap::new();
            if y < self.height as usize {
                let mut x = 0;
                while x < width {
                    if !occupied(x, y) {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < width && occupied(x, y) {
                        x += 1;
                    }
                    let key = (start, x);
                    let first = match active.remove(&key) {
                        Some(first) => first,
                        None => {
                            runs += 1;
                            if runs > MAX_RUNS {
                                return Err("Prepared mask contour budget exceeded".into());
                            }
                            y
                        }
                    };
                    next.insert(key, first);
                }
            }
            for ((start, end), first) in active {
                rectangles.push(Rect::new(start as f64, first as f64, end as f64, y as f64));
            }
            active = next;
        }
        let mut path = BezPath::new();
        for rect in rectangles {
            let rect = if let Some(clip) = &self.clip { rect.intersect(clip.rect()) } else { rect };
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            if self.clip.is_some() && (rect.width() < 0.0001 || rect.height() < 0.0001) {
                return Err("Retained crop leaves a sub-resolution mask rectangle".into());
            }
            path.extend(rect.to_path(0.000001).elements().iter().copied());
        }
        let xf = Affine::new(self.pixel_to_page_mm);
        // Normalize in integer pixel coordinates before placement. A small physical
        // pixel must not disappear in a page-space boolean tolerance.
        let path = PathData::from_bezpath(&path);
        vectorcraft_pathops::try_boolean_for_measurement(
            &path,
            FillRule::NonZero,
            &PathData::default(),
            FillRule::NonZero,
            vectorcraft_pathops::BoolOp::Union,
        )
        .map(|path| path.transformed(xf))
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mask(width: u32, height: u32, bits: &str) -> Mask {
        Mask {
            source_sha256: "a".repeat(64),
            width,
            height,
            page: 0,
            pixel_to_page_mm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            bits_hex: bits.into(),
            clip: None,
        }
    }
    #[test]
    fn exact_hole_and_opaque_masks_preserve_pixel_edges() {
        let hole = mask(3, 3, "f780").path().unwrap();
        let boundary = crate::geometry::Boundary::new(&hole, FillRule::NonZero, 0.000001).unwrap();
        let mut work = 0;
        assert!(boundary.contains_bounded((0.5, 0.5).into(), FillRule::NonZero, &mut work).unwrap());
        assert!(!boundary.contains_bounded((1.5, 1.5).into(), FillRule::NonZero, &mut work).unwrap());
        let opaque = mask(3, 3, "ff80").path().unwrap();
        assert_eq!(opaque.to_bezpath().bounding_box(), Rect::new(0.0, 0.0, 3.0, 3.0));
        assert!(mask(3, 3, "0000").path().unwrap().is_empty());
    }
    #[test]
    fn invalid_packed_masks_and_unbounded_placement_are_refused() {
        for m in [mask(0, 2, ""), mask(3, 3, "ffff"), mask(3, 3, "ff"), mask(1_048_577, 1, "")] {
            assert!(m.path().is_err());
        }
        let mut m = mask(3, 3, "ff80");
        m.pixel_to_page_mm = [0.0; 6];
        assert!(m.path().is_err());
        m.pixel_to_page_mm = [1e6, 0.0, 0.0, 1e6, 0.0, 0.0];
        assert!(m.path().is_err());
    }
    #[test]
    fn excessive_pixel_runs_are_refused_before_boolean_construction() {
        let mut m = mask(512, 128, &"aa".repeat(512 * 128 / 8));
        // Alternate row phases so runs cannot merge vertically.
        let mut bits = String::new();
        for y in 0..128 {
            bits.push_str(&if y % 2 == 0 { "aa" } else { "55" }.repeat(64));
        }
        m.bits_hex = bits;
        assert!(m.path().unwrap_err().contains("contour budget"));
    }
}

#[cfg(test)]
mod crop_tests {
    use super::*;
    #[test]
    fn retained_rational_boundary_survives_serialization_and_mask_geometry() {
        let mut mask = Mask {
            source_sha256: "a".repeat(64),
            width: 3,
            height: 3,
            page: 0,
            pixel_to_page_mm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            bits_hex: "ff80".into(),
            clip: None,
        };
        assert!(serde_json::to_value(&mask).unwrap().get("clip").is_none());
        mask.clip = Some(crate::prepared_crop::Clip::from_raw([19, 18, 40, 54], [36, 0, 0, 36, 18, 18], 3, 3).unwrap());
        assert_eq!(mask.visible_window().unwrap(), Some([0, 0, 2, 3]));
        let p = mask.path().unwrap();
        let b = p.to_bezpath().bounding_box();
        assert!((b.x0 - 1.0 / 12.0).abs() < 0.00001);
        assert!((b.x1 - 11.0 / 6.0).abs() < 0.00001);
        assert_eq!((b.y0, b.y1), (0.0, 3.0));
        let serialized = serde_json::to_string(&mask).unwrap();
        let restored: Mask = serde_json::from_str(&serialized).unwrap();
        assert_eq!(crate::digest(&mask).unwrap(), crate::digest(&restored).unwrap());
        assert_eq!(p, restored.path().unwrap());
        mask.clip.as_mut().unwrap().edges[2] = [1, 1_000_000];
        mask.clip.as_mut().unwrap().edges[0] = [0, 1];
        assert!(mask.path().unwrap_err().contains("sub-resolution"));
    }
}
