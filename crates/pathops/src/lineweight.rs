//! Line weight check: the parts of filled art narrower than a minimum width, and the gaps between
//! parts narrower than it.
//!
//! Print processes have a minimum line weight: art thinner than it doesn't print, and gaps
//! narrower than it fill in. Prepress checks for both by shrinking the art by half the minimum
//! (what breaks apart or vanishes is too thin) and growing it by half (where shapes run together
//! the gap between them is too narrow). In the terms of mathematical morphology, with `r` half the
//! minimum width and the art's silhouette `S` (everything that prints, united):
//!
//! - the **opening** of `S` is `S` shrunk by `r` and grown back by `r` (round joins): every part
//!   of `S` that a disc of diameter `w` can reach while staying inside it. **Thin parts** are
//!   `S` minus its opening.
//! - the **closing** of `S` is `S` grown by `r` and shrunk back by `r`: `S` plus every notch,
//!   hole and gap a disc of diameter `w` can't get into. **Narrow gaps** are the closing minus `S`.
//!
//! Both are computed with [`crate::offset_path`] (round joins) and the curve-preserving booleans.
//!
//! Taken literally, every convex corner is "thin" (the opening rounds it off with radius `r`) and
//! every concave corner a "gap" (the closing fills it in), and the refit of the boolean results
//! leaves numerical slivers along every edge. None of that is what the check is for, so a region
//! is reported only where it runs a distance `reach` beyond the art that is wide enough: a thin
//! region must hold a point farther than `reach` from the opening (`S` minus the opening grown by
//! `reach`), and a gap a point deeper than `reach` inside the closing (the closing shrunk by
//! `reach`). A region whose far part covers less than [`MIN_AREA_FRACTION`] of `w²` is dropped as
//! noise.
//!
//! What `reach` decides (see [`DEFAULT_REACH`]): a dot or a line smaller than `w` with nothing wide
//! around it is always reported; a hairline must stick out more than `reach` from the art it
//! hangs off, a slit run more than `reach` deep, and a hairline bridge or a narrow gap between two
//! shapes run more than `2·reach` along them. A corner of angle `θ` reaches
//! `r·(1/sin(θ/2) − 1)` past the opening (or into the closing): the rounded-off corner of a square
//! `0.41·r`, so it is never reported, while corners sharper than `2·asin(r / (r + reach))` are
//! (about 29° at the default: the tip of such a spike, or the crotch of such a notch, really is
//! narrower than `w` for a length of more than `1.5·w`). At a reach of `r` the crotches of
//! ordinary type (the notches of N, A and R) were reported in their hundreds in customer files;
//! at the default they are left out.
//!
//! Work is bounded: large art is checked tile by tile, and the parts of a tile too far apart to
//! interact cluster by cluster (see [`line_weight`]); the caller can stop between them (a time
//! budget).

use kurbo::{BezPath, Rect, Shape as _};
use linesweeper::topology::{ContourIdx, Contours};
use vectorcraft_geom::{FillRule, PathData, Point};

use crate::boolean::{Arrangement, DEFAULT_PRECISION, Tidy, contours_to_path, fill_bezpath, normalize_bez};
use crate::offset::Join;
use crate::{BoolOp, PathOpsError, interior_point, try_boolean, try_offset_path};

/// A region is reported only when the part of it farther than half the minimum width from the
/// wide-enough art covers at least this fraction of the minimum width squared (see the module
/// documentation). Anything smaller is refit noise.
pub const MIN_AREA_FRACTION: f64 = 0.02;

/// How far, in minimum widths, a region must run beyond the art that is wide enough to be
/// reported when [`LineWeightOptions::new`] picks it (see the module documentation): one and a half
/// minimum widths, which leaves out the crotches and tips of ordinary type and keeps hairlines,
/// slits, gaps and bridges of any real length.
pub const DEFAULT_REACH: f64 = 1.5;

/// What [`line_weight`] checks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineWeightOptions {
    /// The minimum width (document units, points).
    pub min_width: f64,
    /// How far a region must run beyond the art that is wide enough to be reported (points; see
    /// the module documentation).
    pub reach: f64,
    /// Look for narrow gaps as well as thin parts.
    pub gaps: bool,
}

impl LineWeightOptions {
    /// Thin parts and gaps narrower than `min_width`, with the [`DEFAULT_REACH`].
    pub fn new(min_width: f64) -> Self {
        LineWeightOptions { min_width, reach: DEFAULT_REACH * min_width, gaps: true }
    }
}

/// How much the art grows before it is shrunk (points): enough to close the cracks between
/// abutting shapes (see `check_cluster`), far below anything that prints.
const SEAL: f64 = DEFAULT_PRECISION;

/// The miter limit handed to [`crate::offset_path`] (unused with round joins, which the check always
/// uses: a disc is round).
const MITER: f64 = 4.0;

/// The most path segments a tile's art may hold before the tile is split (see
/// [`line_weight`]): each sweep over a tile then stays within a few hundred megabytes and a few
/// seconds.
const TILE_SEGMENTS: usize = 10_000;

/// How many times a tile may be split in four.
const MAX_TILE_DEPTH: usize = 8;

/// What [`line_weight`] found.
#[derive(Clone, Debug, Default)]
pub struct LineWeight {
    /// Parts of the art narrower than the minimum width, one path per region (an outline and its
    /// holes), in no particular order.
    pub thin: Vec<PathData>,
    /// Gaps narrower than the minimum width, one path per region.
    pub gaps: Vec<PathData>,
    /// Tiles the art was split into to bound the work (one for all but large files).
    pub tiles: usize,
    /// Tiles checked before the caller asked to stop (all of them when it didn't).
    pub checked: usize,
    /// The bounds of each piece of art whose geometry couldn't be resolved (left unchecked; rare
    /// near-degenerate input the sweep refuses).
    pub failed: Vec<vectorcraft_geom::Rect>,
}

impl LineWeight {
    /// Were all tiles checked?
    pub fn complete(&self) -> bool {
        self.checked == self.tiles
    }
}

/// Check the silhouette of `shapes` (the union of the filled paths, each under its fill rule)
/// for parts narrower than the minimum width and, with `gaps`, gaps narrower than it.
/// `keep_going` is asked before each piece of work; once it answers false the rest is skipped
/// (see [`LineWeight::complete`]).
///
/// The work is bounded however large the art: what is found at a point depends only on the art
/// within a few minimum widths of it, so art with more than [`TILE_SEGMENTS`] segments is split
/// into tiles, each checked with the art round it out to that distance, the regions found cut to
/// the tile and the pieces of regions that cross tiles joined up again. Within a tile, parts too
/// far apart to interact are checked one cluster at a time.
pub fn line_weight(
    shapes: &[(&PathData, FillRule)],
    options: LineWeightOptions,
    keep_going: impl FnMut() -> bool,
) -> Result<LineWeight, PathOpsError> {
    line_weight_tiled(shapes, options, TILE_SEGMENTS, keep_going)
}

/// [`line_weight`] with tiles of at most `tile_segments` segments.
fn line_weight_tiled(
    shapes: &[(&PathData, FillRule)],
    options: LineWeightOptions,
    tile_segments: usize,
    mut keep_going: impl FnMut() -> bool,
) -> Result<LineWeight, PathOpsError> {
    let LineWeightOptions { min_width, reach, gaps } = options;
    if !(min_width.is_finite() && min_width > 0.0 && reach.is_finite() && reach >= 0.0) {
        return Err(PathOpsError::Degenerate);
    }
    let r = min_width / 2.0;
    let min_area = MIN_AREA_FRACTION * min_width * min_width;
    let resolved = resolve(shapes);
    let Some(bounds) = resolved.iter().map(|s| s.bounds).reduce(|a, b| a.union(b)) else { return Ok(LineWeight::default()) };
    // What is found inside a tile depends on the art up to this far round it: `2r` for the
    // opening and closing, `r + reach` more for the far part of a region, and a little to spare.
    let margin = 3.0 * r + reach + 1e-3 * min_width;
    let tiles = tiles(&resolved, bounds, margin, tile_segments);
    let tiled = tiles.len() > 1;
    let mut out = LineWeight { tiles: tiles.len(), ..Default::default() };
    let tidy = Tidy::free(DEFAULT_PRECISION);
    'tiles: for tile in &tiles {
        if !keep_going() {
            break;
        }
        let area = tiled.then(|| tile.inflate(margin, margin));
        let silhouette = match silhouette(&resolved, area) {
            Ok(Some(s)) => s,
            Ok(None) => {
                out.checked += 1;
                continue;
            }
            Err(_) => {
                // The union itself was refused: the tile goes unchecked, the rest is still checked.
                out.failed.push(area.unwrap_or(bounds));
                out.checked += 1;
                continue;
            }
        };
        let parts = components(&silhouette);
        for cluster in clusters(&parts, r) {
            if !keep_going() {
                break 'tiles;
            }
            let s = contours_to_path(
                &silhouette,
                cluster.iter().flat_map(|&p| parts.get(p).map(|c| c.contours.iter().copied()).into_iter().flatten()),
                &tidy,
            );
            if s.is_empty() {
                continue;
            }
            match check_cluster(&s, r, reach, gaps, min_area).or_else(|_| retry_shifted(&s, r, reach, gaps, min_area)) {
                Ok((thin, narrow)) if tiled => {
                    // Only what lies in the tile itself: the art round it is there to get the
                    // tile right, and the next tile reports its own.
                    let cut = |found: Vec<PathData>| -> Vec<PathData> {
                        let rect = vectorcraft_geom::shapes::rectangle(*tile);
                        found
                            .iter()
                            .filter_map(|p| try_boolean(p, FillRule::NonZero, &rect, FillRule::NonZero, BoolOp::Intersect, DEFAULT_PRECISION).ok())
                            .filter(|p| !p.is_empty())
                            .collect()
                    };
                    out.thin.extend(cut(thin));
                    out.gaps.extend(cut(narrow));
                }
                Ok((thin, narrow)) => {
                    out.thin.extend(thin);
                    out.gaps.extend(narrow);
                }
                Err(_) => out.failed.extend(s.bounds()),
            }
        }
        out.checked += 1;
    }
    if tiled {
        // Regions that cross from one tile into the next come back together.
        // (If the sweep refuses, the pieces are reported as they are: a region crossing tiles then
        // counts once per tile.)
        if let Ok(thin) = join_up(&out.thin) {
            out.thin = thin;
        }
        if let Ok(gaps) = join_up(&out.gaps) {
            out.gaps = gaps;
        }
    }
    Ok(out)
}

/// A shape resolved on its own under its fill rule: outlines and holes, consistently wound, so
/// that any of them can be united with the non-zero rule.
struct Resolved {
    path: BezPath,
    bounds: Rect,
    segments: usize,
}

fn resolve(shapes: &[(&PathData, FillRule)]) -> Vec<Resolved> {
    let mut out = vec![];
    for (p, rule) in shapes {
        let bp = fill_bezpath(p);
        if bp.elements().is_empty() {
            continue;
        }
        // A shape that can't be resolved (non-finite coordinates) is left out, as unite_all does.
        let Ok(c) = normalize_bez(&bp, *rule) else { continue };
        let mut path = BezPath::new();
        for ct in c.contours() {
            path.extend(ct.path.iter());
        }
        if path.elements().is_empty() {
            continue;
        }
        let (bounds, segments) = (path.bounding_box(), path.segments().count());
        out.push(Resolved { path, bounds, segments });
    }
    out
}

/// Do `a` and `b` overlap (edges touching count)?
fn overlaps(a: Rect, b: Rect) -> bool {
    a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1
}

/// Tiles covering `bounds`, each holding (with the art up to `margin` round it) at most
/// `max_segments` segments where splitting can get it there: a tile is split in four while it
/// holds more, unless no quarter would hold markedly less (one shape larger than the tiles) or
/// it is already small.
fn tiles(shapes: &[Resolved], bounds: Rect, margin: f64, max_segments: usize) -> Vec<Rect> {
    let load = |t: Rect| -> usize {
        let around = t.inflate(margin, margin);
        shapes.iter().filter(|s| overlaps(s.bounds, around)).map(|s| s.segments).sum()
    };
    let mut out = vec![];
    let mut todo = vec![(bounds, load(bounds), 0usize)];
    while let Some((t, n, depth)) = todo.pop() {
        let small = t.width().max(t.height()) < 8.0 * margin;
        if n <= max_segments || depth >= MAX_TILE_DEPTH || small {
            out.push(t);
            continue;
        }
        let c = t.center();
        let quarters =
            [Rect::new(t.x0, t.y0, c.x, c.y), Rect::new(c.x, t.y0, t.x1, c.y), Rect::new(t.x0, c.y, c.x, t.y1), Rect::new(c.x, c.y, t.x1, t.y1)];
        let loads = quarters.map(load);
        if loads.iter().all(|&q| q * 10 > n * 9) {
            out.push(t);
            continue;
        }
        todo.extend(quarters.into_iter().zip(loads).map(|(q, l)| (q, l, depth + 1)));
    }
    out
}

/// The union of the resolved shapes (those reaching into `area`, cut to it), as contours; `None`
/// when it is empty.
fn silhouette(shapes: &[Resolved], area: Option<Rect>) -> Result<Option<Contours>, PathOpsError> {
    let mut all = BezPath::new();
    for s in shapes.iter().filter(|s| area.is_none_or(|a| overlaps(s.bounds, a))) {
        all.extend(s.path.iter());
    }
    if all.elements().is_empty() {
        return Ok(None);
    }
    let Some(area) = area else { return normalize_bez(&all, FillRule::NonZero).map(Some) };
    let rect = vectorcraft_geom::shapes::rectangle(area).to_bezpath();
    let arr = Arrangement::new(vec![(all, FillRule::NonZero), (rect, FillRule::NonZero)])?;
    Ok(Some(arr.contours(|m| m.iter().all(|&b| b))))
}

/// `pieces` united, one path per region.
fn join_up(pieces: &[PathData]) -> Result<Vec<PathData>, PathOpsError> {
    let refs: Vec<(&PathData, FillRule)> = pieces.iter().map(|p| (p, FillRule::NonZero)).collect();
    let resolved = resolve(&refs);
    let Some(united) = silhouette(&resolved, None)? else { return Ok(vec![]) };
    let tidy = Tidy::free(DEFAULT_PRECISION);
    Ok(components(&united).iter().map(|c| contours_to_path(&united, c.contours.iter().copied(), &tidy)).collect())
}

/// `path` offset by `delta` with round joins. The sweep refuses rare near-degenerate input (two
/// curves of an earlier offset meeting within rounding), so a refused offset is tried again a
/// hair nearer and farther (a ten-thousandth of the distance, far below any print tolerance)
/// before it fails.
fn offset(path: &PathData, delta: f64) -> Result<PathData, PathOpsError> {
    let mut last = Err(PathOpsError::Degenerate);
    for k in [1.0, 0.9999, 1.0001, 0.999, 1.001] {
        last = try_offset_path(path, delta * k, Join::Round, MITER);
        if last.is_ok() {
            break;
        }
    }
    last
}

/// [`check_cluster`] again with the art moved by a hair and the regions moved back: the sweep
/// refuses rare near-degenerate input (an intersection landing on a vertex within rounding), and
/// a shift far below any print tolerance usually gets past it.
fn retry_shifted(s: &PathData, r: f64, reach: f64, gaps: bool, min_area: f64) -> Result<(Vec<PathData>, Vec<PathData>), PathOpsError> {
    let shift = kurbo::Vec2::new(1.7e-4, 1.1e-4) * (r / 0.28);
    let moved = s.transformed(kurbo::Affine::translate(shift));
    let (thin, narrow) = check_cluster(&moved, r, reach, gaps, min_area)?;
    let back = |v: Vec<PathData>| v.into_iter().map(|p| p.transformed(kurbo::Affine::translate(-shift))).collect();
    Ok((back(thin), back(narrow)))
}

/// Thin parts and (with `gaps`) narrow gaps of one cluster's silhouette `s`.
fn check_cluster(s: &PathData, r: f64, reach: f64, gaps: bool, min_area: f64) -> Result<(Vec<PathData>, Vec<PathData>), PathOpsError> {
    let nz = FillRule::NonZero;
    let difference =
        |a: &PathData, b: &PathData| if b.is_empty() { Ok(a.clone()) } else { try_boolean(a, nz, b, nz, BoolOp::Difference, DEFAULT_PRECISION) };
    // Opening: shrink, grow back. What the shrink loses entirely the grow can't restore.
    // Abutting shapes can leave cracks in their union: slits of no width (a boundary that runs
    // into the art and straight back out) that print as nothing but that a shrink carves round,
    // leaving a false thin sliver along them. Growing by a hair first closes every crack; the
    // shrink takes the hair back, so the opening is otherwise the same.
    let sealed = offset(s, SEAL)?;
    let eroded = offset(&sealed, -(r + SEAL))?;
    let opened = if eroded.is_empty() { PathData::default() } else { offset(&eroded, r)? };
    let thin_all = difference(s, &opened)?;
    let thin = if thin_all.is_empty() {
        vec![]
    } else {
        // The far part: farther than `reach` from the opening (the shrunk art grown by
        // `r + reach`).
        let near = if eroded.is_empty() { PathData::default() } else { offset(&eroded, r + reach)? };
        significant(&thin_all, &difference(&thin_all, &near)?, min_area, r + reach)?
    };
    if !gaps {
        return Ok((thin, vec![]));
    }
    // Closing: grow, shrink back.
    let grown = offset(s, r)?;
    let closed = offset(&grown, -r)?;
    let gaps_all = difference(&closed, s)?;
    let narrow = if gaps_all.is_empty() {
        vec![]
    } else {
        // The far part: deeper than `reach` inside the closing (the grown art shrunk by
        // `r + reach`).
        let core = offset(&grown, -(r + reach))?;
        let far = if core.is_empty() { PathData::default() } else { try_boolean(&gaps_all, nz, &core, nz, BoolOp::Intersect, DEFAULT_PRECISION)? };
        significant(&gaps_all, &far, min_area, r + reach)?
    };
    Ok((thin, narrow))
}

/// The regions of `all` (one path per outline with its holes) that hold a piece of `far` of at
/// least `min_area`, each cut to within `grow` of those pieces.
///
/// The cut keeps a region to the feature that made it: the refit of the offsets leaves slivers a
/// hundredth of a point wide along the edges of the art, and one that touches a real thin part
/// would otherwise make the whole edge part of it (the tip of a long wedge reported as the wedge's
/// whole outline). Everything of a real thin part or gap lies within `r + reach` of its far part.
fn significant(all: &PathData, far: &PathData, min_area: f64, grow: f64) -> Result<Vec<PathData>, PathOpsError> {
    if far.is_empty() {
        return Ok(vec![]);
    }
    // The pieces of `far` big enough to count, and a point inside each.
    let far_contours = normalize_bez(&fill_bezpath(far), FillRule::NonZero)?;
    let tidy = Tidy::free(DEFAULT_PRECISION);
    let pieces: Vec<PathData> = components(&far_contours)
        .iter()
        .map(|c| contours_to_path(&far_contours, c.contours.iter().copied(), &tidy))
        .filter(|p| crate::area(p, FillRule::NonZero) >= min_area)
        .collect();
    let points: Vec<Point> = pieces.iter().filter_map(interior_point).collect();
    if points.is_empty() {
        return Ok(vec![]);
    }
    let zone = offset(&PathData::new(pieces.into_iter().flat_map(|p| p.subpaths).collect()), grow)?;
    let cut = try_boolean(all, FillRule::NonZero, &zone, FillRule::NonZero, BoolOp::Intersect, DEFAULT_PRECISION)?;
    let all_contours = normalize_bez(&fill_bezpath(&cut), FillRule::NonZero)?;
    let mut out = vec![];
    for c in components(&all_contours) {
        let mut region = BezPath::new();
        for &i in &c.contours {
            region.extend(all_contours[i].path.iter());
        }
        let holds = |p: &Point| c.bounds.contains(kurbo::Point::new(p.x, p.y)) && region.winding(kurbo::Point::new(p.x, p.y)) != 0;
        if points.iter().any(holds) {
            out.push(contours_to_path(&all_contours, c.contours.iter().copied(), &tidy));
        }
    }
    Ok(out)
}

/// One connected piece of a filled region: an outline and the holes directly inside it.
struct Component {
    contours: Vec<ContourIdx>,
    bounds: Rect,
}

/// The pieces of `c`: each outer contour with its holes (an island inside a hole is a piece of
/// its own).
fn components(c: &Contours) -> Vec<Component> {
    let all: Vec<_> = c.contours().collect();
    let mut index = vec![usize::MAX; all.len()];
    let mut out: Vec<Component> = vec![];
    for (i, k) in all.iter().enumerate() {
        if k.outer {
            index[i] = out.len();
            out.push(Component { contours: vec![ContourIdx(i)], bounds: k.path.bounding_box() });
        }
    }
    for (i, k) in all.iter().enumerate() {
        if !k.outer
            && let Some(ContourIdx(p)) = k.parent
            && let Some(comp) = index.get(p).and_then(|&j| out.get_mut(j))
        {
            comp.contours.push(ContourIdx(i));
        }
    }
    out
}

/// Groups of the pieces of a silhouette whose bounds come within `2r` of each other (directly or
/// through other pieces): only parts that close can be bridged by a closing of radius `r`, and the
/// opening works on each part alone. Pieces are listed by index into `parts`.
fn clusters(parts: &[Component], r: f64) -> Vec<Vec<usize>> {
    let n = parts.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while let Some(&p) = parent.get(i) {
            if p == i {
                break;
            }
            // Path halving keeps the trees shallow.
            let gp = parent.get(p).copied().unwrap_or(p);
            if let Some(slot) = parent.get_mut(i) {
                *slot = gp;
            }
            i = gp;
        }
        i
    }
    let margin = r * 1.001 + 1e-9;
    let grown: Vec<Rect> = parts.iter().map(|p| p.bounds.inflate(margin, margin)).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| grown[a].x0.total_cmp(&grown[b].x0));
    // Sweep along x: each piece is compared with the pieces after it that start before it ends.
    for (k, &a) in order.iter().enumerate() {
        let ra = grown[a];
        for &b in order.iter().skip(k + 1) {
            let rb = grown[b];
            if rb.x0 > ra.x1 {
                break;
            }
            if rb.y0 <= ra.y1 && ra.y0 <= rb.y1 {
                let (x, y) = (find(&mut parent, a), find(&mut parent, b));
                if x != y
                    && let Some(slot) = parent.get_mut(x)
                {
                    *slot = y;
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    groups.into_values().collect()
}

#[cfg(test)]
mod tests {
    use vectorcraft_geom::{Rect as GRect, shapes};

    use super::*;

    const MM: f64 = 72.0 / 25.4;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> PathData {
        shapes::rectangle(GRect::new(x * MM, y * MM, (x + w) * MM, (y + h) * MM))
    }

    fn check(shapes: &[PathData], min_mm: f64) -> LineWeight {
        let refs: Vec<(&PathData, FillRule)> = shapes.iter().map(|p| (p, FillRule::NonZero)).collect();
        line_weight(&refs, LineWeightOptions::new(min_mm * MM), || true).unwrap()
    }

    fn bounds_mm(p: &PathData) -> [f64; 4] {
        let b = p.bounds().unwrap();
        [b.x0 / MM, b.y0 / MM, b.x1 / MM, b.y1 / MM]
    }

    #[test]
    fn a_clean_square_has_nothing_thin_and_no_gaps() {
        let r = check(&[rect(0.0, 0.0, 5.0, 5.0)], 0.2);
        assert!(r.thin.is_empty() && r.gaps.is_empty(), "{r:?}");
        assert!(r.complete());
    }

    #[test]
    fn a_hairline_touching_a_square_is_thin_at_its_minimum_only() {
        // A 1 mm square and a 0.1 mm wide, 2 mm long line running off its right side.
        let art = [rect(0.0, 0.0, 1.0, 1.0), rect(1.0, 0.45, 2.0, 0.1)];
        let r = check(&art, 0.2);
        assert_eq!(r.thin.len(), 1, "{r:?}");
        let b = bounds_mm(&r.thin[0]);
        // The region is the line (it may reach a hair into the square where the two meet).
        assert!(b[0] > 0.8 && (b[2] - 3.0).abs() < 0.01 && (b[1] - 0.45).abs() < 0.01 && (b[3] - 0.55).abs() < 0.01, "{b:?}");
        assert!(r.gaps.is_empty(), "{:?}", r.gaps.iter().map(bounds_mm).collect::<Vec<_>>());
        let r = check(&art, 0.05);
        assert!(r.thin.is_empty() && r.gaps.is_empty(), "{r:?}");
    }

    #[test]
    fn two_squares_close_together_leave_one_narrow_gap() {
        let art = [rect(0.0, 0.0, 1.0, 1.0), rect(1.1, 0.0, 1.0, 1.0)];
        let r = check(&art, 0.2);
        assert!(r.thin.is_empty(), "{r:?}");
        assert_eq!(r.gaps.len(), 1, "{r:?}");
        let b = bounds_mm(&r.gaps[0]);
        assert!((b[0] - 1.0).abs() < 0.01 && (b[2] - 1.1).abs() < 0.01 && b[1] < 0.05 && b[3] > 0.95, "{b:?}");
        // Wider apart than the minimum: no gap, and they are separate clusters.
        let r = check(&[rect(0.0, 0.0, 1.0, 1.0), rect(1.5, 0.0, 1.0, 1.0)], 0.2);
        assert!(r.gaps.is_empty(), "{r:?}");
    }

    #[test]
    fn a_dot_smaller_than_the_minimum_is_thin() {
        let r = check(&[rect(0.0, 0.0, 0.15, 0.15), rect(3.0, 3.0, 2.0, 2.0)], 0.2);
        assert_eq!(r.thin.len(), 1, "{r:?}");
        assert!(bounds_mm(&r.thin[0])[2] < 0.2);
    }

    #[test]
    fn a_narrow_slot_cut_into_a_shape_is_a_gap() {
        // A 3 mm square with a 0.1 mm slot cut 1.5 mm into it from the top.
        let square = rect(0.0, 0.0, 3.0, 3.0);
        let slot = rect(1.45, -1.0, 0.1, 2.5);
        let art = crate::boolean(&square, FillRule::NonZero, &slot, FillRule::NonZero, BoolOp::Difference);
        let r = check(&[art], 0.2);
        assert!(r.thin.is_empty(), "{:?}", r.thin.iter().map(bounds_mm).collect::<Vec<_>>());
        assert_eq!(r.gaps.len(), 1, "{r:?}");
    }

    /// An isosceles triangle with a 3 mm base and apex angle `deg`, apex up.
    fn spike(deg: f64) -> PathData {
        let h = 1.5 / (deg.to_radians() / 2.0).tan();
        let pts = [(0.0, h), (3.0, h), (1.5, 0.0)].map(|(x, y)| vectorcraft_geom::Point::new(x * MM, y * MM));
        PathData::single(vectorcraft_geom::SubPath::polyline(&pts, true))
    }

    #[test]
    fn a_sharp_tip_is_thin_once_it_is_sharper_than_the_reach_allows() {
        let min = 0.2 * MM;
        let run = |art: PathData, reach: f64| {
            let opts = LineWeightOptions { reach: reach * min, ..LineWeightOptions::new(min) };
            line_weight(&[(&art, FillRule::NonZero)], opts, || true).unwrap().thin.len()
        };
        // At the default reach (1.5 × min) corners sharper than about 29° are reported…
        assert_eq!(run(spike(20.0), DEFAULT_REACH), 1);
        assert_eq!(run(spike(40.0), DEFAULT_REACH), 0);
        // …and at half of min, those sharper than about 60°.
        assert_eq!(run(spike(40.0), 0.5), 1);
        assert_eq!(run(spike(75.0), 0.5), 0);
        // The tip reported is at the apex.
        let art = spike(20.0);
        let found = line_weight(&[(&art, FillRule::NonZero)], LineWeightOptions::new(min), || true).unwrap();
        let b = found.thin[0].bounds().unwrap();
        assert!(b.y0.abs() < 0.01 && ((b.x0 + b.x1) / 2.0 - 1.5 * MM).abs() < 0.01, "{b:?}");
    }

    #[test]
    fn the_tip_of_a_long_wedge_is_reported_not_its_whole_edge() {
        // A 60 mm wedge 3 mm wide at its base: narrower than 0.2 mm for its last 4 mm. Refit
        // slivers along its long edges must not make the whole wedge the region.
        let pts = [(0.0, 0.0), (60.0, -1.5), (60.0, 1.5)].map(|(x, y)| vectorcraft_geom::Point::new(x * MM, y * MM));
        let art = PathData::single(vectorcraft_geom::SubPath::polyline(&pts, true));
        let found = line_weight(&[(&art, FillRule::NonZero)], LineWeightOptions::new(0.2 * MM), || true).unwrap();
        assert_eq!(found.thin.len(), 1, "{found:?}");
        let b = bounds_mm(&found.thin[0]);
        assert!(b[0].abs() < 0.01 && b[2] > 3.5 && b[2] < 6.0, "{b:?}");
    }

    #[test]
    fn a_stop_between_clusters_is_reported() {
        let art = [rect(0.0, 0.0, 1.0, 1.0), rect(5.0, 0.0, 1.0, 1.0), rect(10.0, 0.0, 1.0, 1.0)];
        let refs: Vec<(&PathData, FillRule)> = art.iter().map(|p| (p, FillRule::NonZero)).collect();
        let mut asked = 0;
        let r = line_weight(&refs, LineWeightOptions::new(0.2 * MM), || {
            asked += 1;
            asked <= 1
        })
        .unwrap();
        // Asked before the tile, then before each of its three clusters: stopped at the first.
        assert_eq!((r.tiles, r.checked, r.complete()), (1, 0, false));
    }

    #[test]
    fn tiles_find_what_one_pass_finds_and_join_regions_that_cross_them() {
        // A grid of hairlines, squares close together and long hairlines and slits that cross
        // many tiles once the tiles are small.
        let mut art = vec![];
        for i in 0..6 {
            let x = 6.0 * f64::from(i);
            art.push(rect(x, 0.0, 1.0, 1.0));
            art.push(rect(x + 1.0, 0.45, 2.0, 0.1));
            art.push(rect(x, 3.0, 1.0, 1.0));
            art.push(rect(x + 1.1, 3.0, 1.0, 1.0));
        }
        art.push(rect(0.0, 6.0, 36.0, 0.1));
        let slab = rect(0.0, 8.0, 36.0, 3.0);
        let slit = rect(1.0, 9.45, 34.0, 0.1);
        art.push(crate::boolean(&slab, FillRule::NonZero, &slit, FillRule::NonZero, BoolOp::Difference));
        let refs: Vec<(&PathData, FillRule)> = art.iter().map(|p| (p, FillRule::NonZero)).collect();
        let opts = LineWeightOptions::new(0.2 * MM);
        let whole = line_weight_tiled(&refs, opts, usize::MAX, || true).unwrap();
        let tiled = line_weight_tiled(&refs, opts, 40, || true).unwrap();
        assert_eq!(whole.tiles, 1);
        assert!(tiled.tiles >= 8, "{}", tiled.tiles);
        assert!(tiled.complete() && tiled.failed.is_empty());
        assert_eq!((whole.thin.len(), whole.gaps.len()), (7, 7), "6 stubs and the long line; 6 gaps and the slit");
        assert_eq!((tiled.thin.len(), tiled.gaps.len()), (whole.thin.len(), whole.gaps.len()));
        let total = |v: &[PathData]| v.iter().map(|p| crate::area(p, FillRule::NonZero)).sum::<f64>();
        for (a, b) in [(total(&whole.thin), total(&tiled.thin)), (total(&whole.gaps), total(&tiled.gaps))] {
            assert!((a - b).abs() < 1e-3 * a, "{a} {b}");
        }
    }

    #[test]
    fn a_bad_minimum_is_an_error() {
        let art = [rect(0.0, 0.0, 1.0, 1.0)];
        let refs: Vec<(&PathData, FillRule)> = art.iter().map(|p| (p, FillRule::NonZero)).collect();
        for w in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(line_weight(&refs, LineWeightOptions::new(w), || true).is_err(), "{w}");
        }
        let negative = LineWeightOptions { reach: -1.0, ..LineWeightOptions::new(1.0) };
        assert!(line_weight(&refs, negative, || true).is_err());
    }
}
