//! Independent local boundary measurement in millimetres. No maker offset code is used.
//! Cubic Béziers are enclosed by chord-distance bounds of their control hull. Directed
//! boundary maxima are bounded using the 1-Lipschitz distance-to-set function. Every contour
//! participates; no area-based defect suppression, alignment or best-fit registration occurs.
//! This boundary comparator can resolve finer separation than a Boolean sweep.
//! A construction must account for its own sweep resolution separately; this
//! primitive does not certify the topology of any preceding Boolean operation.
use crate::Status;
use serde::{Deserialize, Serialize};
use vectorcraft_geom::{PathData, Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Segment {
    a: Point,
    b: Point,
}
impl Segment {
    fn length(self) -> f64 {
        self.a.distance(self.b)
    }
    fn distance(self, p: Point) -> f64 {
        let d = self.b - self.a;
        let len = d.hypot();
        if len == 0.0 {
            return p.distance(self.a);
        }
        let u = d / len;
        let along = ((p - self.a).dot(u)).clamp(0.0, len);
        p.distance(self.a + u * along)
    }
}
// Spatial bounds change only search cost, never geometry or the error envelope.
#[derive(Clone, Debug)]
struct SegmentIndex {
    bounds: Rect,
    kind: IndexKind,
}
#[derive(Clone, Debug)]
enum IndexKind {
    Leaf(Vec<Segment>),
    Branch(Box<SegmentIndex>, Box<SegmentIndex>),
}
impl SegmentIndex {
    fn new(mut segments: Vec<Segment>) -> Self {
        let bounds = segments.iter().map(|s| Rect::from_points(s.a, s.b)).reduce(|a, b| a.union(b)).unwrap_or(Rect::ZERO);
        if segments.len() <= 8 {
            return Self { bounds, kind: IndexKind::Leaf(segments) };
        }
        let x = bounds.width() >= bounds.height();
        segments.sort_by(|a, b| {
            let midpoint = |s: &Segment| if x { s.a.x + s.b.x } else { s.a.y + s.b.y };
            midpoint(a).total_cmp(&midpoint(b))
        });
        let right = segments.split_off(segments.len() / 2);
        Self { bounds, kind: IndexKind::Branch(Box::new(Self::new(segments)), Box::new(Self::new(right))) }
    }
    fn lower_distance(&self, p: Point) -> f64 {
        let dx = (self.bounds.x0 - p.x).max(0.0).max(p.x - self.bounds.x1);
        let dy = (self.bounds.y0 - p.y).max(0.0).max(p.y - self.bounds.y1);
        dx.hypot(dy)
    }
    fn distance(&self, p: Point, best: &mut f64, budget: &mut usize) -> Result<(), String> {
        *budget += 1;
        if *budget > MAX_EVALUATIONS {
            return Err("Boundary distance budget exceeded".into());
        }
        if self.lower_distance(p) > *best {
            return Ok(());
        }
        match &self.kind {
            IndexKind::Leaf(segments) => {
                for s in segments {
                    *budget += 1;
                    if *budget > MAX_EVALUATIONS {
                        return Err("Boundary distance budget exceeded".into());
                    }
                    *best = best.min(s.distance(p));
                }
            }
            IndexKind::Branch(a, b) => {
                let (a, b) = if a.lower_distance(p) <= b.lower_distance(p) { (a, b) } else { (b, a) };
                a.distance(p, best, budget)?;
                b.distance(p, best, budget)?;
            }
        }
        Ok(())
    }
    fn nearest_segment(&self, p: Point, best: &mut (f64, Option<Segment>), budget: &mut usize) -> Result<(), String> {
        *budget += 1;
        if *budget > MAX_EVALUATIONS {
            return Err("Boundary distance budget exceeded".into());
        }
        if self.lower_distance(p) > best.0 {
            return Ok(());
        }
        match &self.kind {
            IndexKind::Leaf(segments) => {
                for segment in segments {
                    *budget += 1;
                    if *budget > MAX_EVALUATIONS {
                        return Err("Boundary distance budget exceeded".into());
                    }
                    let distance = segment.distance(p);
                    if distance < best.0 {
                        *best = (distance, Some(*segment));
                    }
                }
            }
            IndexKind::Branch(a, b) => {
                let (a, b) = if a.lower_distance(p) <= b.lower_distance(p) { (a, b) } else { (b, a) };
                a.nearest_segment(p, best, budget)?;
                b.nearest_segment(p, best, budget)?;
            }
        }
        Ok(())
    }
    fn minimum_to_segment(&self, segment: Segment, best: &mut f64, budget: &mut usize) -> Result<(), String> {
        *budget += 1;
        if *budget > MAX_EVALUATIONS {
            return Err("Boundary distance budget exceeded".into());
        }
        let a = Rect::from_points(segment.a, segment.b);
        let b = self.bounds;
        let dx = (a.x0 - b.x1).max(b.x0 - a.x1).max(0.0);
        let dy = (a.y0 - b.y1).max(b.y0 - a.y1).max(0.0);
        if dx.hypot(dy) > *best {
            return Ok(());
        }
        match &self.kind {
            IndexKind::Leaf(segments) => {
                for other in segments {
                    *budget += 1;
                    if *budget > MAX_EVALUATIONS {
                        return Err("Boundary distance budget exceeded".into());
                    }
                    let distance = if intersects(segment, *other, 0.0) {
                        0.0
                    } else {
                        segment.distance(other.a).min(segment.distance(other.b)).min(other.distance(segment.a)).min(other.distance(segment.b))
                    };
                    *best = best.min(distance);
                }
            }
            IndexKind::Branch(a, b) => {
                a.minimum_to_segment(segment, best, budget)?;
                b.minimum_to_segment(segment, best, budget)?;
            }
        }
        Ok(())
    }
    fn near(&self, p: Point, radius: f64, budget: &mut usize) -> Result<bool, String> {
        *budget += 1;
        if *budget > MAX_EVALUATIONS {
            return Err("Boundary distance budget exceeded".into());
        }
        if self.lower_distance(p) > radius {
            return Ok(false);
        }
        match &self.kind {
            IndexKind::Leaf(segments) => {
                for s in segments {
                    *budget += 1;
                    if *budget > MAX_EVALUATIONS {
                        return Err("Boundary distance budget exceeded".into());
                    }
                    if s.distance(p) <= radius {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            IndexKind::Branch(a, b) => Ok(a.near(p, radius, budget)? || b.near(p, radius, budget)?),
        }
    }
    fn winding(&self, p: Point) -> i32 {
        if p.y < self.bounds.y0 || p.y >= self.bounds.y1 || p.x > self.bounds.x1 {
            return 0;
        }
        match &self.kind {
            IndexKind::Leaf(segments) => segments
                .iter()
                .map(|s| {
                    let cross = (s.b - s.a).cross(p - s.a);
                    if s.a.y <= p.y && s.b.y > p.y && cross > 0.0 {
                        1
                    } else if s.a.y > p.y && s.b.y <= p.y && cross < 0.0 {
                        -1
                    } else {
                        0
                    }
                })
                .sum(),
            IndexKind::Branch(a, b) => a.winding(p) + b.winding(p),
        }
    }
    fn winding_bounded(&self, p: Point, budget: &mut usize) -> Result<i32, String> {
        *budget = budget.saturating_add(1);
        if *budget > MAX_EVALUATIONS {
            return Err("Boundary winding budget exceeded".into());
        }
        if p.y < self.bounds.y0 || p.y >= self.bounds.y1 || p.x > self.bounds.x1 {
            return Ok(0);
        }
        match &self.kind {
            IndexKind::Leaf(segments) => {
                *budget = budget.saturating_add(segments.len());
                if *budget > MAX_EVALUATIONS {
                    return Err("Boundary winding budget exceeded".into());
                }
                Ok(self.winding(p))
            }
            IndexKind::Branch(a, b) => Ok(a.winding_bounded(p, budget)? + b.winding_bounded(p, budget)?),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Boundary {
    retained_segments: usize,
    segments: Vec<Segment>,
    index: SegmentIndex,
    winding_index: SegmentIndex,
    error: f64,
    components: usize,
    holes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measurement {
    pub status: Status,
    pub lower_mm: f64,
    pub upper_mm: f64,
    pub uncertainty_mm: f64,
    pub expected_components: usize,
    pub observed_components: usize,
    pub expected_holes: usize,
    pub observed_holes: usize,
    pub witness_mm: Option<[f64; 2]>,
    pub evaluations: usize,
    pub reason: String,
}
const MAX_SEGMENTS: usize = 100_000;
const MAX_EVALUATIONS: usize = 20_000_000;
const UNRESOLVED_CONTACT: &str = "Intersecting or unresolved touching contours require planar coverage resolution";
fn midpoint(a: Point, b: Point) -> Point {
    a + (b - a) * 0.5
}
fn cubic(points: [Point; 4], precision: f64, depth: u8, out: &mut Vec<Point>) -> Result<(), String> {
    if out.len() >= MAX_SEGMENTS {
        return Err("Boundary subdivision budget exceeded".into());
    }
    let [a, b, c, d] = points;
    let line = Segment { a, b: d };
    if line.distance(b).max(line.distance(c)) <= precision {
        out.push(d);
        return Ok(());
    }
    if depth >= 32 {
        return Err("Curve uncertainty cannot be resolved within depth budget".into());
    }
    let ab = midpoint(a, b);
    let bc = midpoint(b, c);
    let cd = midpoint(c, d);
    let abc = midpoint(ab, bc);
    let bcd = midpoint(bc, cd);
    let m = midpoint(abc, bcd);
    cubic([a, ab, abc, m], precision, depth + 1, out)?;
    cubic([m, bcd, cd, d], precision, depth + 1, out)
}
fn winding(ring: &[Point], p: Point) -> i32 {
    let mut n = 0;
    for pair in ring.windows(2) {
        let [a, b] = [pair[0], pair[1]];
        let cross = (b - a).cross(p - a);
        if a.y <= p.y && b.y > p.y && cross > 0.0 {
            n += 1;
        } else if a.y > p.y && b.y <= p.y && cross < 0.0 {
            n -= 1;
        }
    }
    n
}
fn intersects(a: Segment, b: Segment, error: f64) -> bool {
    if a.a.x.max(a.b.x) + error < b.a.x.min(b.b.x)
        || b.a.x.max(b.b.x) + error < a.a.x.min(a.b.x)
        || a.a.y.max(a.b.y) + error < b.a.y.min(b.b.y)
        || b.a.y.max(b.b.y) + error < a.a.y.min(a.b.y)
    {
        return false;
    }
    let ab = a.b - a.a;
    let cd = b.b - b.a;
    let c1 = ab.cross(b.a - a.a);
    let c2 = ab.cross(b.b - a.a);
    let c3 = cd.cross(a.a - b.a);
    let c4 = cd.cross(a.b - b.a);
    (c1.signum() != c2.signum() && c3.signum() != c4.signum())
        || [a.distance(b.a), a.distance(b.b), b.distance(a.a), b.distance(a.b)].into_iter().any(|d| d <= error)
}
impl Boundary {
    pub(crate) fn linear_segments(&self) -> impl Iterator<Item = (Point, Point)> + '_ {
        self.segments.iter().map(|s| (s.a, s.b))
    }
    pub(crate) fn linear_error_mm(&self) -> f64 {
        self.error
    }

    pub fn new(path: &PathData, fill_rule: vectorcraft_geom::FillRule, precision_mm: f64) -> Result<Self, String> {
        let mut precision = precision_mm;
        for attempt in 0..=12 {
            match Self::at_precision(path, fill_rule, precision) {
                Err(reason) if reason == UNRESOLVED_CONTACT && attempt < 12 && precision > 1e-12 => precision *= 0.25,
                result => return result,
            }
        }
        Err(UNRESOLVED_CONTACT.into())
    }
    // Short, valid neighbouring curve pieces can overlap coarse error envelopes. Refine the
    // numerical enclosure, never delete the pieces or waive a genuine crossing/touching contour.
    fn at_precision(path: &PathData, fill_rule: vectorcraft_geom::FillRule, precision_mm: f64) -> Result<Self, String> {
        if !precision_mm.is_finite() || precision_mm <= 0.0 || precision_mm > 1.0 {
            return Err("Positive finite numerical precision in mm required".into());
        }
        let mut rings = vec![];
        let mut flattened_segments = 0usize;
        let mut maximum = 1.0f64;
        for sub in &path.subpaths {
            if sub.anchors.is_empty() {
                continue;
            }
            if !sub.closed || sub.anchors.len() < 2 {
                return Err("Boundary measurement requires closed filled paths".into());
            }
            for a in &sub.anchors {
                for p in [a.p, a.h_in, a.h_out] {
                    if !p.x.is_finite() || !p.y.is_finite() || p.x.abs() > 1e6 || p.y.abs() > 1e6 {
                        return Err("Nonfinite/out-of-range geometry".into());
                    }
                    maximum = maximum.max(p.x.abs()).max(p.y.abs());
                }
            }
            let first = sub.anchors.first().ok_or("Empty contour")?;
            let mut points = vec![first.p];
            for (a, b) in sub.anchors.iter().zip(sub.anchors.iter().cycle().skip(1)).take(sub.anchors.len()) {
                cubic([a.p, a.h_out, b.h_in, b.p], precision_mm, 0, &mut points)?;
                if flattened_segments.saturating_add(points.len().saturating_sub(1)) > MAX_SEGMENTS {
                    return Err("Boundary segment budget exceeded".into());
                }
            }
            points.dedup();
            if points.len() < 4 {
                return Err("Degenerate contour cannot establish topology".into());
            }
            flattened_segments = flattened_segments.saturating_add(points.len().saturating_sub(1));
            rings.push(points);
        }
        let error = precision_mm + 64.0 * f64::EPSILON * maximum;
        let mut segments = vec![];
        let mut owners = vec![];
        for (r, ring) in rings.iter().enumerate() {
            for (i, p) in ring.windows(2).enumerate() {
                segments.push(Segment { a: p[0], b: p[1] });
                owners.push((r, i));
            }
        }
        if segments.len() > MAX_SEGMENTS {
            return Err("Boundary segment budget exceeded".into());
        }
        // Crossing/touching contours need a qualified planar arrangement first. Never mistake
        // overlapping source components or cancelled winding edges for physical boundaries.
        let mut budget = 0;
        let mut sweep: Vec<_> = (0..segments.len()).collect();
        sweep.sort_by(|&a, &b| segments[a].a.x.min(segments[a].b.x).total_cmp(&segments[b].a.x.min(segments[b].b.x)));
        for (position, &i) in sweep.iter().enumerate() {
            let a = segments[i];
            for &j in sweep.iter().skip(position + 1) {
                let b = segments[j];
                if b.a.x.min(b.b.x) > a.a.x.max(a.b.x) + error * 2.0 {
                    break;
                }
                if a.a.y.max(a.b.y) + error * 2.0 < b.a.y.min(b.b.y) || b.a.y.max(b.b.y) + error * 2.0 < a.a.y.min(a.b.y) {
                    continue;
                }
                budget += 1;
                if budget > MAX_EVALUATIONS {
                    return Err("Topology intersection budget exceeded".into());
                }
                let (ar, ai) = owners[i];
                let (br, bi) = owners[j];
                if ar == br && (ai.abs_diff(bi) == 1 || (ai.min(bi) == 0 && ai.max(bi) + 2 == rings[ar].len())) {
                    continue;
                }
                if intersects(a, b, error * 2.0) {
                    return Err(UNRESOLVED_CONTACT.into());
                }
            }
        }
        let signs: Vec<i32> = rings
            .iter()
            .map(|ring| {
                let area: f64 = ring.windows(2).map(|p| p[0].to_vec2().cross(p[1].to_vec2())).sum();
                if area > 0.0 { 1 } else { -1 }
            })
            .collect();
        let mut components = 0;
        let mut holes = 0;
        let mut visible = std::collections::BTreeSet::new();
        for (i, ring) in rings.iter().enumerate() {
            let p = *ring.first().ok_or("Empty contour")?;
            let mut outside = 0i32;
            for (j, other) in rings.iter().enumerate() {
                if j == i {
                    continue;
                }
                budget = budget.saturating_add(other.len().saturating_sub(1));
                if budget > MAX_EVALUATIONS {
                    return Err("Topology winding budget exceeded".into());
                }
                outside += winding(other, p);
            }
            let inside = outside + signs[i];
            let fill = |w: i32| match fill_rule {
                vectorcraft_geom::FillRule::NonZero => w != 0,
                vectorcraft_geom::FillRule::EvenOdd => w.abs() % 2 == 1,
            };
            match (fill(outside), fill(inside)) {
                (false, true) => {
                    components += 1;
                    visible.insert(i);
                }
                (true, false) => {
                    holes += 1;
                    visible.insert(i);
                }
                _ => {}
            }
        }
        let winding_segments = segments.len();
        let winding_index = SegmentIndex::new(segments.clone());
        let segments: Vec<_> = segments.into_iter().zip(owners).filter(|(_, (r, _))| visible.contains(r)).map(|(s, _)| s).collect();
        // Keep all rings for the original fill rule's winding; hidden internal edges aren't measured.
        let retained_segments = winding_segments.saturating_add(segments.len().saturating_mul(2));
        Ok(Self { retained_segments, index: SegmentIndex::new(segments.clone()), segments, winding_index, error, components, holes })
    }
    fn indexed_nearest(&self, p: Point, budget: &mut usize) -> Result<(f64, Option<Segment>), String> {
        let mut best = (f64::INFINITY, None);
        self.index.nearest_segment(p, &mut best, budget)?;
        Ok(best)
    }
    #[cfg(test)]
    fn nearest(&self, p: Point, budget: &mut usize) -> Result<(f64, usize), String> {
        let mut best = (f64::INFINITY, 0);
        for (i, s) in self.segments.iter().enumerate() {
            *budget += 1;
            if *budget > MAX_EVALUATIONS {
                return Err("Boundary distance budget exceeded".into());
            }
            let d = s.distance(p);
            if d < best.0 {
                best = (d, i);
            }
        }
        Ok(best)
    }
    pub fn contains(&self, p: Point, fill: vectorcraft_geom::FillRule) -> bool {
        let w = self.winding_index.winding(p);
        match fill {
            vectorcraft_geom::FillRule::NonZero => w != 0,
            vectorcraft_geom::FillRule::EvenOdd => w.abs() % 2 == 1,
        }
    }
    /// Includes the visible boundary and both spatial-index copies.
    pub fn retained_segments(&self) -> usize {
        self.retained_segments
    }
    pub fn contains_bounded(&self, p: Point, fill: vectorcraft_geom::FillRule, budget: &mut usize) -> Result<bool, String> {
        let w = self.winding_index.winding_bounded(p, budget)?;
        Ok(match fill {
            vectorcraft_geom::FillRule::NonZero => w != 0,
            vectorcraft_geom::FillRule::EvenOdd => w.abs() % 2 == 1,
        })
    }
    /// Conservative pixel-edge envelope: never discard a potentially near curve.
    pub fn near_boundary(&self, p: Point, radius: f64, budget: &mut usize) -> Result<bool, String> {
        if !radius.is_finite() || radius < 0.0 {
            return Err("Invalid boundary envelope".into());
        }
        self.index.near(p, radius + self.error, budget)
    }
    /// Distance interval to the original curved boundary; shared by the raster error envelope.
    pub fn distance_interval(&self, p: Point, budget: &mut usize) -> Result<(f64, f64), String> {
        let mut distance = f64::INFINITY;
        self.index.distance(p, &mut distance, budget)?;
        Ok(((distance - self.error).max(0.0), distance + self.error))
    }
}
fn directed(a: &Boundary, b: &Boundary, precision: f64, budget: &mut usize) -> Result<(f64, f64, Option<Point>), String> {
    let mut lower = 0.0f64;
    let mut upper = 0.0f64;
    let mut witness = None;
    for s in &a.segments {
        let da = b.indexed_nearest(s.a, budget)?;
        let db = b.indexed_nearest(s.b, budget)?;
        let mut pending = vec![(*s, da, db, 0u8)];
        while let Some((segment, da, db, depth)) = pending.pop() {
            let lo = da.0.max(db.0);
            let hi = if da.1 == db.1 {
                lo
            } else {
                // Distance to any fixed target segment is convex along this line.
                // Either endpoint witness therefore supplies a certified upper
                // bound, often tighter than the general Lipschitz envelope.
                let fixed = |s: Option<Segment>| s.map(|s| s.distance(segment.a).max(s.distance(segment.b))).unwrap_or(f64::INFINITY);
                ((da.0 + db.0 + segment.length()) * 0.5).min(fixed(da.1)).min(fixed(db.1)).max(lo)
            };
            if lo > lower {
                lower = lo;
                witness = Some(if da.0 >= db.0 { segment.a } else { segment.b });
            }
            if hi <= lower + precision {
                upper = upper.max(hi);
                continue;
            }
            if depth >= 32 {
                return Err("Boundary maximum remains numerically unresolved".into());
            }
            let m = midpoint(segment.a, segment.b);
            let dm = b.indexed_nearest(m, budget)?;
            pending.push((Segment { a: segment.a, b: m }, da, dm, depth + 1));
            pending.push((Segment { a: m, b: segment.b }, dm, db, depth + 1));
        }
    }
    Ok((lower, upper.max(lower), witness))
}
/// Observable one-way boundary gap. It does not prove complete morphological
/// equivalence, source completeness or topology preservation.
#[derive(Clone, Debug, Serialize)]
pub struct GapRange {
    pub minimum_mm: [f64; 2],
    pub maximum_mm: [f64; 2],
    pub uncertainty_mm: f64,
    /// Local diagnostic only: traversal work can differ across floating-point targets.
    #[serde(skip_serializing)]
    pub evaluations: usize,
}
pub fn boundary_gap(from: &PathData, to: &PathData, precision: f64) -> Result<GapRange, String> {
    let a = Boundary::new(from, vectorcraft_geom::FillRule::NonZero, precision)?;
    let b = Boundary::new(to, vectorcraft_geom::FillRule::NonZero, precision)?;
    if a.segments.is_empty() || b.segments.is_empty() {
        return Err("Boundary gap requires two nonempty physical boundaries".into());
    }
    let mut budget = 0;
    let max = directed(&a, &b, precision, &mut budget)?;
    let mut minimum = f64::INFINITY;
    for segment in &a.segments {
        b.index.minimum_to_segment(*segment, &mut minimum, &mut budget)?;
    }
    let error = a.error + b.error;
    let minimum = [(minimum - error).max(0.0), minimum + error];
    let maximum = [(max.0 - error).max(0.0), max.1 + error];
    Ok(GapRange {
        minimum_mm: minimum,
        maximum_mm: maximum,
        uncertainty_mm: (minimum[1] - minimum[0]).max(maximum[1] - maximum[0]),
        evaluations: budget,
    })
}
/// Tolerance is an explicitly approved production criterion; precision is a numerical bound.
/// An interval crossing the criterion is Incomplete, never rounded into Pass.
pub fn compare(expected: &PathData, observed: &PathData, tolerance_mm: f64, precision_mm: f64) -> Result<Measurement, String> {
    if !tolerance_mm.is_finite() || tolerance_mm < 0.0 {
        return Err("Explicit nonnegative acceptance required".into());
    }
    let a = Boundary::new(expected, vectorcraft_geom::FillRule::NonZero, precision_mm)?;
    let b = Boundary::new(observed, vectorcraft_geom::FillRule::NonZero, precision_mm)?;
    let mut evaluations = 0;
    let empty = a.segments.is_empty() || b.segments.is_empty();
    if empty {
        let same = a.segments.is_empty() && b.segments.is_empty();
        return Ok(Measurement {
            status: if same { Status::Pass } else { Status::Fail },
            lower_mm: 0.0,
            upper_mm: 0.0,
            uncertainty_mm: 0.0,
            expected_components: a.components,
            observed_components: b.components,
            expected_holes: a.holes,
            observed_holes: b.holes,
            witness_mm: None,
            evaluations,
            reason: if same { "Both explicitly measured regions are empty" } else { "Required coverage missing or unexpected coverage present" }
                .into(),
        });
    }
    let ab = directed(&a, &b, precision_mm, &mut evaluations)?;
    let ba = directed(&b, &a, precision_mm, &mut evaluations)?;
    let error = a.error + b.error;
    let lower = (ab.0.max(ba.0) - error).max(0.0);
    let upper = ab.1.max(ba.1) + error;
    let topology = a.components != b.components || a.holes != b.holes;
    let status = if topology || lower > tolerance_mm {
        Status::Fail
    } else if upper <= tolerance_mm {
        Status::Pass
    } else {
        Status::Incomplete
    };
    let witness = if ab.0 >= ba.0 { ab.2 } else { ba.2 };
    Ok(Measurement {
        status,
        lower_mm: lower,
        upper_mm: upper,
        uncertainty_mm: upper - lower,
        expected_components: a.components,
        observed_components: b.components,
        expected_holes: a.holes,
        observed_holes: b.holes,
        witness_mm: witness.map(|p| [p.x, p.y]),
        evaluations,
        reason: if topology {
            "Component/hole topology differs; no defect-area suppression"
        } else if status == Status::Incomplete {
            "Measurement interval overlaps acceptance; refine precision"
        } else {
            "Every contour checked in both directions without alignment"
        }
        .into(),
    })
}
/// Select actual coincident boundary portions. A nearby edge is not a shared edge.
/// Curved contacts use the same bounded subdivision; numerically ambiguous contact is refused.
pub fn selected_edges(source: &PathData, neighbours: &PathData, shared: bool, precision: f64) -> Result<PathData, String> {
    use vectorcraft_geom::{Anchor, FillRule, SubPath};
    let source = Boundary::new(source, FillRule::NonZero, precision)?;
    let neighbours = Boundary::new(neighbours, FillRule::NonZero, precision)?;
    let mut result = vec![];
    let mut budget = 0;
    for a in &source.segments {
        let d = a.b - a.a;
        let length = d.hypot();
        if length == 0.0 {
            continue;
        }
        let u = d / length;
        let mut intervals = vec![];
        for b in &neighbours.segments {
            budget += 1;
            if budget > MAX_EVALUATIONS {
                return Err("Shared-boundary budget exceeded".into());
            }
            // Preserve exact endpoint identities. Reprojecting a coincident endpoint through
            // a normalized vector can leave a one-ULP gap, which a round cap would enlarge.
            if (a.a == b.a && a.b == b.b) || (a.a == b.b && a.b == b.a) {
                intervals.push((0.0, length));
                continue;
            }
            let project = |p: Point| {
                if p == a.a {
                    0.0
                } else if p == a.b {
                    length
                } else {
                    (p - a.a).dot(u)
                }
            };
            let lo = project(b.a).min(project(b.b)).max(0.0);
            let hi = project(b.a).max(project(b.b)).min(length);
            if hi <= lo {
                continue;
            }
            let ca = (b.a - a.a).cross(u);
            let cb = (b.b - a.a).cross(u);
            if ca == 0.0 && cb == 0.0 {
                intervals.push((lo, hi));
            } else if ca.abs().max(cb.abs()) <= source.error + neighbours.error {
                return Err("Shared contact is within numerical uncertainty; refine or supply exact prepared boundary".into());
            }
        }
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f64, f64)> = vec![];
        for (lo, hi) in intervals {
            if let Some(last) = merged.last_mut().filter(|last| lo <= last.1) {
                last.1 = last.1.max(hi);
            } else {
                merged.push((lo, hi));
            }
        }
        let intervals = if shared {
            merged
        } else {
            let mut out = vec![];
            let mut at = 0.0;
            for (lo, hi) in merged {
                if lo > at {
                    out.push((at, lo));
                }
                at = hi;
            }
            if at < length {
                out.push((at, length));
            }
            out
        };
        for (lo, hi) in intervals {
            let point = |t: f64| {
                if t == 0.0 {
                    a.a
                } else if t == length {
                    a.b
                } else {
                    a.a + u * t
                }
            };
            result.push(SubPath { anchors: vec![Anchor::corner(point(lo)), Anchor::corner(point(hi))], closed: false });
        }
    }
    Ok(PathData::new(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_geom::{Affine, BezPath};
    fn rectangle(x: f64, y: f64, w: f64, h: f64) -> PathData {
        let mut p = BezPath::new();
        p.move_to((x, y));
        p.line_to((x + w, y));
        p.line_to((x + w, y + h));
        p.line_to((x, y + h));
        p.close_path();
        PathData::from_bezpath(&p)
    }
    #[test]
    fn measurement_reports_round_trip_and_gap_work_stays_local() {
        let shape = rectangle(0.0, 0.0, 10.0, 10.0);
        let measurement = compare(&shape, &shape, 0.01, 0.0001).unwrap();
        let value = serde_json::to_value(&measurement).unwrap();
        let restored: Measurement = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&restored).unwrap(), value);
        let gap = boundary_gap(&rectangle(1.0, 1.0, 8.0, 8.0), &shape, 0.0001).unwrap();
        assert!(gap.evaluations > 0);
        let value = serde_json::to_value(&gap).unwrap();
        assert!(value.get("evaluations").is_none());
        assert!(value.get("minimum_mm").is_some());
        assert!(value.get("maximum_mm").is_some());
    }
    #[test]
    fn spatial_search_matches_exhaustive_curved_boundary_and_keeps_tiny_holes() {
        use vectorcraft_geom::Shape;
        let mut path = PathData::from_bezpath(&vectorcraft_geom::kurbo::Circle::new((5.0, 5.0), 4.0).to_path(0.000001));
        let hole = PathData::from_bezpath(&vectorcraft_geom::kurbo::Circle::new((5.0, 5.0), 0.01).to_path(0.000001));
        path.subpaths.extend(hole.subpaths);
        let boundary = Boundary::new(&path, vectorcraft_geom::FillRule::EvenOdd, 0.00001).unwrap();
        assert_eq!(boundary.holes, 1);
        let mut indexed_cost = 0;
        let mut linear_cost = 0;
        for x in 0..31 {
            for y in 0..31 {
                let p = Point::new(x as f64 / 3.0, y as f64 / 3.0);
                let (distance, _) = boundary.nearest(p, &mut linear_cost).unwrap();
                let (lo, hi) = boundary.distance_interval(p, &mut indexed_cost).unwrap();
                assert!(lo <= distance && distance <= hi);
                let expected = boundary.segments.iter().any(|s| s.distance(p) <= 0.02 + boundary.error);
                assert_eq!(boundary.near_boundary(p, 0.02, &mut indexed_cost).unwrap(), expected);
            }
        }
        assert!(indexed_cost < linear_cost / 2);
    }
    #[test]
    fn directional_gap_measures_inner_boundary_and_detects_crossing_without_waiving_topology() {
        let outer = rectangle(0.0, 0.0, 10.0, 10.0);
        let inner = rectangle(0.2, 0.2, 9.6, 9.6);
        let gap = boundary_gap(&inner, &outer, 0.0001).unwrap();
        for range in [gap.minimum_mm, gap.maximum_mm] {
            assert!(range[0] <= 0.2 && range[1] >= 0.2, "{range:?}");
            assert!(range[1] - range[0] < 0.001);
        }
        let moved = inner.transformed(Affine::translate((1.0, 0.0)));
        let changed = boundary_gap(&moved, &outer, 0.0001).unwrap();
        assert!(changed.minimum_mm[0] <= 0.0001 && changed.minimum_mm[1] < 0.001);
        assert!(changed.maximum_mm[0] > 0.79);
        assert!(boundary_gap(&PathData::default(), &outer, 0.0001).is_err());
        // A line is not a printable filled boundary and cannot become a zero-gap success.
        let degenerate = rectangle(1.0, 1.0, 1.0, 0.0);
        assert!(boundary_gap(&degenerate, &outer, 0.0001).is_err());
    }
    #[test]
    fn indexed_nearest_matches_every_linear_witness() {
        let p = rectangle(0.0, 0.0, 10.0, 10.0);
        let boundary = Boundary::new(&p, vectorcraft_geom::FillRule::NonZero, 0.0001).unwrap();
        for x in -2..13 {
            for y in -2..13 {
                let point = Point::new(f64::from(x), f64::from(y));
                let a = boundary.nearest(point, &mut 0).unwrap();
                let b = boundary.indexed_nearest(point, &mut 0).unwrap();
                assert_eq!(a.0, b.0);
                assert_eq!(b.1.unwrap().distance(point), a.0);
            }
        }
    }
    #[test]
    fn acceptance_interval_does_not_round_a_borderline_deviation_into_pass() {
        let a = rectangle(0.0, 0.0, 10.0, 10.0);
        for (shift, expected) in [(0.09, Status::Pass), (0.1, Status::Incomplete), (0.11, Status::Fail)] {
            let b = a.transformed(Affine::translate((shift, 0.0)));
            let m = compare(&a, &b, 0.1, 0.001).unwrap();
            assert_eq!(m.status, expected, "{m:?}");
            assert!(m.lower_mm <= shift && m.upper_mm >= shift);
        }
    }
    #[test]
    fn small_holes_and_islands_are_not_discarded_by_area() {
        let a = rectangle(0.0, 0.0, 10.0, 10.0);
        let mut b = a.clone();
        b.subpaths.extend(rectangle(5.0, 5.0, 0.01, 0.01).subpaths);
        b.subpaths.last_mut().unwrap().anchors.reverse();
        let m = compare(&a, &b, 1.0, 0.0001).unwrap();
        assert_eq!(m.status, Status::Fail);
        assert_eq!(m.observed_holes, 1);
        let mut b = a.clone();
        b.subpaths.extend(rectangle(10.01, 5.0, 0.01, 0.01).subpaths);
        let m = compare(&a, &b, 1.0, 0.0001).unwrap();
        assert_eq!(m.status, Status::Fail);
        assert_eq!(m.observed_components, 2);
    }
    #[test]
    fn local_defects_cannot_hide_in_a_good_average() {
        let a = rectangle(0.0, 0.0, 100.0, 100.0);
        let mut bp = BezPath::new();
        bp.move_to((0.0, 0.0));
        bp.line_to((50.0, 0.0));
        bp.line_to((50.0, -0.3));
        bp.line_to((50.02, 0.0));
        bp.line_to((100.0, 0.0));
        bp.line_to((100.0, 100.0));
        bp.line_to((0.0, 100.0));
        bp.close_path();
        let m = compare(&a, &PathData::from_bezpath(&bp), 0.1, 0.0001).unwrap();
        assert_eq!(m.status, Status::Fail);
        assert!(m.lower_mm > 0.29);
    }
    #[test]
    fn curves_are_bounded_and_internal_nonzero_contours_are_not_holes() {
        let circle = vectorcraft_geom::kurbo::Circle::new((0.0, 0.0), 5.0);
        use vectorcraft_geom::Shape;
        let a = PathData::from_bezpath(&circle.to_path(0.00001));
        let b = a.transformed(Affine::translate((0.05, 0.0)));
        let m = compare(&a, &b, 0.1, 0.001).unwrap();
        assert_eq!(m.status, Status::Pass);
        assert!(m.lower_mm <= 0.05 && m.upper_mm >= 0.05);
        let mut a = rectangle(0.0, 0.0, 10.0, 10.0);
        a.subpaths.extend(rectangle(2.0, 2.0, 1.0, 1.0).subpaths);
        let b = rectangle(0.0, 0.0, 10.0, 10.0);
        assert_eq!(compare(&a, &b, 0.1, 0.001).unwrap().status, Status::Pass);
    }
    #[test]
    fn unresolved_intersections_and_invalid_inputs_are_errors_not_empty_success() {
        let mut a = rectangle(0.0, 0.0, 10.0, 10.0);
        a.subpaths.extend(rectangle(5.0, 5.0, 10.0, 10.0).subpaths);
        assert!(compare(&a, &a, 0.1, 0.001).is_err());
        assert!(compare(&PathData::default(), &a, f64::NAN, 0.001).is_err());
    }
    #[test]
    fn winding_is_bounded_without_changing_holes_or_fill_rules() {
        let mut path = rectangle(0.0, 0.0, 10.0, 10.0);
        path.subpaths.extend(rectangle(3.0, 3.0, 4.0, 4.0).subpaths);
        let boundary = Boundary::new(&path, vectorcraft_geom::FillRule::EvenOdd, 0.000001).unwrap();
        for (point, expected) in [(Point::new(1.0, 1.0), true), (Point::new(4.0, 4.0), false), (Point::new(11.0, 1.0), false)] {
            assert_eq!(boundary.contains_bounded(point, vectorcraft_geom::FillRule::EvenOdd, &mut 0).unwrap(), expected);
        }
        let mut exhausted = MAX_EVALUATIONS;
        assert!(
            boundary
                .contains_bounded(Point::new(1.0, 1.0), vectorcraft_geom::FillRule::EvenOdd, &mut exhausted)
                .unwrap_err()
                .contains("winding budget")
        );
        assert_eq!(boundary.retained_segments(), 24);
    }
    #[test]
    fn all_contours_share_flattening_and_topology_work_bounds() {
        let many = |count| {
            let mut p = PathData::default();
            for i in 0..count {
                p.subpaths.extend(rectangle(i as f64 * 3.0, 0.0, 1.0, 1.0).subpaths);
            }
            p
        };
        assert!(Boundary::new(&many(MAX_SEGMENTS / 4 + 1), vectorcraft_geom::FillRule::NonZero, 0.000001).unwrap_err().contains("segment budget"));
        assert!(Boundary::new(&many(2500), vectorcraft_geom::FillRule::NonZero, 0.000001).unwrap_err().contains("Topology winding budget"));
    }
}

#[cfg(test)]
mod short_edge_regression {
    use super::*;
    #[test]
    fn short_collinear_edges_refine_without_deleting_them_or_accepting_a_touch() {
        let mut b = vectorcraft_geom::BezPath::new();
        b.move_to((0.0, 0.0));
        b.line_to((1.0, 0.0));
        b.line_to((1.0, 1.0));
        b.line_to((0.5, 1.0));
        b.line_to((0.5 - 2.0e-9, 1.0));
        b.line_to((0.5 - 4.0e-9, 1.0));
        b.line_to((0.0, 1.0));
        b.close_path();
        let p = PathData::from_bezpath(&b);
        let count = p.anchor_count();
        let boundary = Boundary::new(&p, vectorcraft_geom::FillRule::NonZero, 0.0001).unwrap();
        assert_eq!(boundary.components, 1);
        assert!(boundary.error < 1.0e-9);
        assert_eq!(boundary.linear_segments().count(), count);
        b.move_to((0.5, 1.0));
        b.line_to((0.6, 1.1));
        b.line_to((0.4, 1.1));
        b.close_path();
        assert!(Boundary::new(&PathData::from_bezpath(&b), vectorcraft_geom::FillRule::NonZero, 0.0001).is_err());
    }
}

#[cfg(test)]
mod curved_refinement_regression {
    use super::*;
    use vectorcraft_geom::{FillRule, Shape, kurbo::Circle};
    fn circles(radius: f64, gap: f64) -> (PathData, PathData, PathData) {
        let a = PathData::from_bezpath(&Circle::new((0.0, 0.0), radius).to_path(0.000001));
        let b = PathData::from_bezpath(&Circle::new((2.0 * radius + gap, 0.0), radius).to_path(0.000001));
        let mut combined = a.clone();
        combined.subpaths.extend(b.subpaths.clone());
        (a, b, combined)
    }
    #[test]
    fn curved_tangency_refuses_and_resolved_neighbours_survive_boolean() {
        let (_, _, touch) = circles(1.0, 0.0);
        assert!(Boundary::new(&touch, FillRule::NonZero, 0.0001).is_err());
        let (a, b, p) = circles(1.0, 0.00001);
        assert_eq!(Boundary::new(&p, FillRule::NonZero, 0.0001).unwrap().components, 2);
        let result =
            vectorcraft_pathops::try_boolean_for_measurement(&a, FillRule::NonZero, &b, FillRule::NonZero, vectorcraft_pathops::BoolOp::Union)
                .unwrap();
        assert_eq!(Boundary::new(&result, FillRule::NonZero, 0.0001).unwrap().components, 2);
    }
    #[test]
    fn refinement_still_refuses_when_curves_exceed_the_segment_budget() {
        let (_, _, touch) = circles(1000.0, 0.0);
        assert_eq!(Boundary::at_precision(&touch, FillRule::NonZero, 0.0001).unwrap_err(), UNRESOLVED_CONTACT);
        let error = Boundary::new(&touch, FillRule::NonZero, 0.0001).unwrap_err();
        assert!(error.contains("budget"), "{error}");
    }
}
