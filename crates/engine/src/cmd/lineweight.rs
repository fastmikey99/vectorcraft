//! Line weight check (`lineweight.check`, `lineweight.clear`): the parts of the artwork narrower
//! than a print process's minimum line weight, and the gaps narrower than it.
//!
//! Line weight is judged on the whole artwork's silhouette, not object by object: two thin lines
//! side by side can print as one wide one, and a gap between two objects fills in whatever the
//! objects are. So the check unites everything that prints in the scope — fills, strokes as their
//! outlines (dashes, caps, arrowheads and width profiles as drawn; brushes as their art), type as
//! its glyph outlines, live objects and symbols as their art, clipped by their clipping paths — and
//! hands the union to [`vectorcraft_pathops::line_weight`], which finds the thin parts by shrinking
//! and regrowing it (a morphological opening) and the narrow gaps by growing and shrinking it back
//! (a closing). Hidden objects, template and non-printing layers and guides are left out, as
//! printing leaves them out. Raster images can't be measured as vectors: they are skipped and
//! counted.
//!
//! With `mark`, the regions are drawn on a layer of their own on top of the others, thin parts in
//! magenta and narrow gaps in cyan. That layer is a locked template layer, so it never prints and
//! no export writes it (exports leave template layers out); running the check again replaces it,
//! and `lineweight.clear` removes it.

use std::sync::Arc;

use serde_json::{Value, json};
use vectorcraft_color::{Color, Paint};
use vectorcraft_doc::appearance::{FillLayer, StrokeLayer};
use vectorcraft_doc::{Appearance, AppearanceItem, Document, LayerColor, Node, NodeId, NodeKind};
use vectorcraft_geom::{FillRule, PathData, Rect};
use vectorcraft_pathops as po;
use vectorcraft_render::effects;

use super::pathops::{len_param, outlined_stroke};
use super::*;

/// The name of the layer the marks go on (and how `lineweight.clear` finds it).
pub const LAYER_NAME: &str = "Lineweight Check";

/// The minimum width when `min` is left out: 0.2 mm, the smallest of the processes' minimums.
const DEFAULT_MIN: f64 = 0.2 * 72.0 / 25.4;

/// The silhouette is refused past this many path segments: the sweep's memory grows with them,
/// and a file this size is a raster trace or a halftone, not line art to check.
pub const MAX_SEGMENTS: usize = 400_000;

/// The time budget when `timeLimit` is left out (seconds), and the most it can be.
const DEFAULT_TIME_LIMIT: f64 = 120.0;
const MAX_TIME_LIMIT: f64 = 3600.0;

/// Regions listed in the result when `maxRegions` is left out, and the most it can be. The counts
/// are always complete.
const DEFAULT_MAX_REGIONS: usize = 200;
const MAX_REGIONS: usize = 10_000;

/// With `paths`, region outlines stop being listed once this much path data has been written.
const MAX_PATH_TEXT: usize = 1_000_000;

/// Thin parts are marked in magenta and narrow gaps in cyan, the colours prepress already reads
/// them in.
fn thin_color() -> Color {
    Color::rgb(1.0, 0.0, 1.0)
}
fn gap_color() -> Color {
    Color::rgb(0.0, 1.0, 1.0)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "lineweight.check",
            "Check Line Weight",
            [],
            None,
            "{min?: pt or a length with its unit (\"0.2 mm\"; default 0.2 mm), ids?: objects to check (default: the whole document), reach?: pt or a length (default 1.5 × min: how far a region must run beyond the art that is wide enough to count; corners sharper than about 29° are reported at the default, about 60° at 0.5 × min), gaps?: bool (default true: also gaps narrower than min), mark?: bool (default false: draw the regions on a locked template layer \"Lineweight Check\" on top, thin parts magenta and gaps cyan, replacing an earlier one; one undo step; it never prints or exports), paths?: bool (default false: each listed region's outline as SVG path data `d`, capped), maxRegions?: count of regions listed per kind (default 200, at most 10000; the counts are always complete), timeLimit?: seconds (default 120)} the parts of the printing art narrower than min (it breaks up when shrunk by half of min) and the gaps narrower than it (they close when it grows by half of min), judged on the union of everything that prints → {min, minMm, reach, thin: {count, regions: [{bounds: [x0, y0, x1, y1], area: pt², d?}]}, gaps: {count, regions}, checkedObjects, skippedImages, segments, complete, stopped?: \"time\"|\"segments\", failed?: [[x0, y0, x1, y1]] (bounds of art whose geometry couldn't be resolved, left unchecked), ms, marked?: layer id}",
            has_doc,
            check
        ),
        cmd!(
            "lineweight.clear",
            "Clear Line Weight Check",
            [],
            None,
            "{} remove the layer lineweight.check marks on (one undo step when there is one) → {removed}",
            has_doc,
            clear
        ),
    ]
}

/// The art the check measures, gathered from the document.
#[derive(Default)]
struct Silhouette {
    /// Filled areas, each under its fill rule.
    shapes: Vec<(PathData, FillRule)>,
    /// Objects of the document whose paint went in.
    objects: usize,
    /// Visible raster images left out.
    images: usize,
    /// Path segments gathered so far.
    segments: usize,
    /// Gathering stopped at [`MAX_SEGMENTS`].
    capped: bool,
}

/// Gathers the [`Silhouette`] of a document's art.
struct Gather<'a> {
    doc: &'a Document,
    brushes: Vec<vectorcraft_brush::Brush>,
    /// Somewhere to make stroke art in (it needs fresh ids, which mean nothing here).
    scratch: Document,
    out: Silhouette,
    /// How deep in symbols and live objects the walk is (a symbol whose art holds an instance of
    /// itself would otherwise never end).
    depth: usize,
}

/// How deep symbols and live objects may nest before the walk stops going in.
const MAX_DEPTH: usize = 64;

impl Gather<'_> {
    /// Add `shape` under `rule`, unless the segment budget is spent.
    fn push(&mut self, shape: PathData, rule: FillRule, out: &mut Vec<(PathData, FillRule)>) {
        if shape.is_empty() || self.out.capped {
            return;
        }
        let segments: usize = shape.subpaths.iter().map(|s| s.anchors.len()).sum();
        if self.out.segments + segments > MAX_SEGMENTS {
            self.out.capped = true;
            return;
        }
        self.out.segments += segments;
        out.push((shape, rule));
    }

    /// The printing art of `n` into `out`. `count`: `n` is an object of the document (not art
    /// made from one, such as the paths of outlined type), counted in [`Silhouette::objects`].
    fn node(&mut self, n: &Node, count: bool, out: &mut Vec<(PathData, FillRule)>) {
        if !n.visible || n.opacity <= 0.0 || self.out.capped {
            return;
        }
        match &n.kind {
            NodeKind::Layer { template: true, .. } | NodeKind::Layer { printable: false, .. } => {}
            NodeKind::Layer { children, clip, .. } | NodeKind::Group { children, clip } => self.children(children, *clip, count, out),
            NodeKind::Path { guide: true, .. } | NodeKind::Path { clipping: true, .. } => {}
            NodeKind::Path { .. } | NodeKind::Compound { .. } => self.leaf(n, count, out),
            NodeKind::Text(_) => {
                if let Some(art) = effects::outline_text(n) {
                    let before = out.len();
                    self.node(&art, false, out);
                    self.out.objects += usize::from(count && out.len() > before);
                }
            }
            NodeKind::Image(_) => self.out.images += 1,
            NodeKind::SymbolInstance { symbol, xf } => {
                let Some(sym) = self.doc.symbols.iter().find(|s| s.name == *symbol).filter(|_| self.depth < MAX_DEPTH) else { return };
                let mut art = (*sym.art).clone();
                art.transform(*xf, false);
                self.depth += 1;
                self.node(&art, count, out);
                self.depth -= 1;
            }
            NodeKind::Blend { .. }
            | NodeKind::Envelope { .. }
            | NodeKind::Mesh(_)
            | NodeKind::Repeat(_)
            | NodeKind::PlacedDocument(_)
            | NodeKind::Intertwine { .. } => {
                if self.depth >= MAX_DEPTH {
                    return;
                }
                self.depth += 1;
                let before = out.len();
                for part in effects::expand_live(Some(self.doc), n) {
                    // Live evaluation hands back plain art (a non-live node is returned as it is,
                    // which would recurse for ever).
                    if vectorcraft_doc::live::is_live(&part) && part.id == n.id {
                        continue;
                    }
                    self.node(&part, false, out);
                }
                self.depth -= 1;
                self.out.objects += usize::from(count && out.len() > before);
            }
        }
    }

    /// The children of a layer or group; with `clip`, the first one clips the rest.
    fn children(&mut self, children: &[Arc<Node>], clip: bool, count: bool, out: &mut Vec<(PathData, FillRule)>) {
        let Some((first, rest)) = children.split_first().filter(|_| clip) else {
            for c in children {
                self.node(c, count, out);
            }
            return;
        };
        let mut inner = vec![];
        for c in rest {
            self.node(c, count, &mut inner);
        }
        if inner.is_empty() {
            return;
        }
        // Nothing to clip by hides the clipped art, as on the canvas.
        let Some((outline, rule)) = effects::clip_outline(first) else { return };
        let clip_path = PathData::from_bezpath(&outline);
        let Some(clip_bounds) = clip_path.bounds() else { return };
        // The common case — a rectangle round art that lies inside it (PDFs clip everything to the
        // page) — needs no work: the art passes through as it is.
        let inside = inner.iter().all(|(p, _)| p.bounds().is_none_or(|b| contains(clip_bounds, b)));
        let rectangular =
            (po::area(&clip_path, rule) - clip_bounds.width() * clip_bounds.height()).abs() <= 1e-6 * clip_bounds.width() * clip_bounds.height();
        if inside && rectangular {
            out.extend(inner);
            return;
        }
        let refs: Vec<(&PathData, FillRule)> = inner.iter().map(|(p, r)| (p, *r)).collect();
        let united = po::unite_all(&refs);
        let clipped = po::boolean(&united, FillRule::NonZero, &clip_path, rule, po::BoolOp::Intersect);
        // The clipped art replaces the art gathered for it: count its segments instead.
        self.out.segments =
            self.out.segments.saturating_sub(inner.iter().map(|(p, _)| p.subpaths.iter().map(|s| s.anchors.len()).sum::<usize>()).sum());
        self.push(clipped, FillRule::NonZero, out);
    }

    /// A path or compound path: each visible fill as the path, each visible stroke as the area it
    /// paints (its outline, or its brush art).
    fn leaf(&mut self, n: &Node, count: bool, out: &mut Vec<(PathData, FillRule)>) {
        let Some((path, rule)) = super::pathops::node_path(n) else { return };
        let before = out.len();
        for item in n.appearance.items.iter().filter(|i| effects::paints(i)) {
            match item {
                AppearanceItem::Fill(_) => self.push(path.clone(), rule, out),
                AppearanceItem::Stroke(st) if st.brush.is_some() => {
                    if let Some(art) = outlined_stroke(&mut self.scratch, &self.brushes, &path, rule, st) {
                        self.node(&art, false, out);
                    }
                }
                AppearanceItem::Stroke(st) => self.push(effects::stroke::outline_region(&path, rule, st), FillRule::NonZero, out),
            }
        }
        self.out.objects += usize::from(count && out.len() > before);
    }
}

/// Does `outer` contain `inner`?
fn contains(outer: Rect, inner: Rect) -> bool {
    outer.x0 <= inner.x0 && outer.y0 <= inner.y0 && inner.x1 <= outer.x1 && inner.y1 <= outer.y1
}

/// Is any ancestor of `id` (or `id` itself) hidden, a template or a non-printing layer?
fn hidden(d: &Document, id: NodeId) -> bool {
    let Some(chain) = d.ancestry(id) else { return true };
    chain
        .iter()
        .chain(std::iter::once(&id))
        .filter_map(|a| d.node(*a))
        .any(|n| !n.visible || matches!(n.kind, NodeKind::Layer { template: true, .. } | NodeKind::Layer { printable: false, .. }))
}

/// The printing art of `doc` (or of the objects `ids` in it).
fn gather(doc: &Document, ids: Option<&[NodeId]>) -> Silhouette {
    // Live geometry effects baked into plain paths, as the exporters see them.
    let baked = effects::bake_document(doc);
    let doc = baked.as_ref().unwrap_or(doc);
    let mut g = Gather { doc, brushes: vectorcraft_brush::library(doc), scratch: Document::new(1.0, 1.0), out: Silhouette::default(), depth: 0 };
    let mut shapes = vec![];
    match ids {
        None => {
            for l in &doc.layers {
                g.node(l, true, &mut shapes);
            }
        }
        Some(ids) => {
            for id in ids {
                if let Some(n) = doc.node(*id).filter(|_| !hidden(doc, *id)) {
                    g.node(n, true, &mut shapes);
                }
            }
        }
    }
    g.out.shapes = shapes;
    g.out
}

/// A clock for the time budget and the `ms` of the result (none on the web, which has no clock).
struct Clock {
    #[cfg(not(target_arch = "wasm32"))]
    start: std::time::Instant,
    limit: f64,
}

impl Clock {
    fn new(limit: f64) -> Self {
        Clock {
            #[cfg(not(target_arch = "wasm32"))]
            start: std::time::Instant::now(),
            limit,
        }
    }
    /// Seconds since the start.
    fn elapsed(&self) -> Option<f64> {
        #[cfg(not(target_arch = "wasm32"))]
        return Some(self.start.elapsed().as_secs_f64());
        #[cfg(target_arch = "wasm32")]
        None
    }
    fn within_budget(&self) -> bool {
        self.elapsed().is_none_or(|t| t < self.limit)
    }
}

/// A region as the result lists it.
fn region_json(p: &PathData, with_path: bool, text_budget: &mut usize) -> Value {
    let round = |v: f64| (v * 1000.0).round() / 1000.0;
    let b = p.bounds().unwrap_or_default();
    let mut v = json!({
        "bounds": [round(b.x0), round(b.y0), round(b.x1), round(b.y1)],
        "area": round(po::area(p, FillRule::NonZero)),
    });
    if with_path {
        let d = p.to_bezpath().to_svg();
        if d.len() <= *text_budget {
            *text_budget -= d.len();
            v["d"] = json!(d);
        }
    }
    v
}

/// The regions of one kind, largest first, as the result lists them.
fn regions_json(regions: &[(PathData, f64)], max: usize, with_path: bool, text_budget: &mut usize) -> Value {
    json!({
        "count": regions.len(),
        "regions": regions.iter().take(max).map(|(p, _)| region_json(p, with_path, text_budget)).collect::<Vec<_>>(),
    })
}

fn check(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "lineweight.check";
    let min = match p.get("min") {
        None | Some(Value::Null) => DEFAULT_MIN,
        Some(_) => {
            len_param(p, "min").ok_or_else(|| bad(C, "min must be a length: a number of points or a string with a unit, such as \"0.2 mm\""))?
        }
    };
    if !(min > 0.0 && min <= 72.0) {
        return Err(bad(C, "min must be more than 0 and at most 1 inch"));
    }
    let reach = match p.get("reach") {
        None | Some(Value::Null) => po::DEFAULT_REACH * min,
        Some(_) => {
            len_param(p, "reach").ok_or_else(|| bad(C, "reach must be a length: a number of points or a string with a unit, such as \"0.3 mm\""))?
        }
    };
    if !(0.0..=72.0).contains(&reach) {
        return Err(bad(C, "reach must be from 0 to 1 inch"));
    }
    let limit = num_or(p, "timeLimit", DEFAULT_TIME_LIMIT, C)?;
    if !(limit > 0.0 && limit <= MAX_TIME_LIMIT) {
        return Err(bad(C, "timeLimit must be more than 0 and at most 3600 seconds"));
    }
    let max_regions = num_or(p, "maxRegions", DEFAULT_MAX_REGIONS as f64, C)?;
    if !(0.0..=MAX_REGIONS as f64).contains(&max_regions) {
        return Err(bad(C, "maxRegions must be from 0 to 10000"));
    }
    let ids = if p.get("ids").is_some_and(|v| !v.is_null()) { Some(checked_ids_param(s, p, "ids", C)?) } else { None };
    let (gaps, mark, with_path) = (bool_or(p, "gaps", true), bool_or(p, "mark", false), bool_or(p, "paths", false));

    let clock = Clock::new(limit);
    let art = gather(&s.doc()?.doc, ids.as_deref());
    let refs: Vec<(&PathData, FillRule)> = art.shapes.iter().map(|(p, r)| (p, *r)).collect();
    let found = if art.capped {
        // Too big to check: say so rather than run out of memory.
        po::LineWeight::default()
    } else {
        po::line_weight(&refs, po::LineWeightOptions { min_width: min, reach, gaps }, || clock.within_budget())
            .map_err(|e| EngineError::Other(format!("Line weight check failed: {e}")))?
    };
    let by_area = |v: Vec<PathData>| {
        let mut v: Vec<(PathData, f64)> = v
            .into_iter()
            .map(|p| {
                let a = po::area(&p, FillRule::NonZero);
                (p, a)
            })
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v
    };
    let (complete, timed_out) = (!art.capped && found.complete() && found.failed.is_empty(), !found.complete());
    let (thin, narrow) = (by_area(found.thin), by_area(found.gaps));
    let marked = if mark { mark_regions(s, &thin, &narrow)? } else { None };
    let mut budget = MAX_PATH_TEXT;
    let max = max_regions as usize;
    let mut out = json!({
        "min": (min * 1e4).round() / 1e4,
        "minMm": (min * 25.4 / 72.0 * 1e4).round() / 1e4,
        "reach": (reach * 1e4).round() / 1e4,
        "thin": regions_json(&thin, max, with_path, &mut budget),
        "gaps": regions_json(&narrow, max, with_path, &mut budget),
        "checkedObjects": art.objects,
        "skippedImages": art.images,
        "segments": art.segments,
        "complete": complete,
        "ms": clock.elapsed().map(|t| (t * 1000.0).round()),
    });
    if art.capped {
        out["stopped"] = json!("segments");
    } else if timed_out {
        out["stopped"] = json!("time");
    }
    if !found.failed.is_empty() {
        out["failed"] = json!(found.failed.iter().map(|b| [b.x0, b.y0, b.x1, b.y1]).collect::<Vec<_>>());
    }
    if let Some(id) = marked {
        out["marked"] = json!(id.0);
    }
    Ok(out)
}

/// A number param with a default, refused when it is there but isn't a finite number.
fn num_or(p: &Value, key: &str, default: f64, cmd: &str) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad(cmd, format!("{key} must be a number"))),
    }
}

/// The top-level layers the check marks on.
fn check_layers(d: &Document) -> Vec<NodeId> {
    d.layers.iter().filter(|l| l.is_layer() && l.name.as_deref() == Some(LAYER_NAME)).map(|l| l.id).collect()
}

/// A mark: region `p` filled and outlined (so a hairline still shows when zoomed out) in `color`.
fn mark_path(d: &mut Document, p: &PathData, color: Color, name: &str) -> Arc<Node> {
    let items = vec![AppearanceItem::Fill(FillLayer::new(Paint::solid(color))), AppearanceItem::Stroke(StrokeLayer::new(Paint::solid(color), 0.5))];
    let mut n = Node::path(d.alloc_id(), p.clone(), Appearance { items, ..Default::default() });
    n.name = Some(name.into());
    Arc::new(n)
}

/// Replace the check layer by one marking `thin` and `gaps` (none when both are empty), in one
/// undo step; → the new layer. Nothing changes (no undo step) when there is neither an old layer
/// nor anything to mark.
fn mark_regions(s: &mut Session, thin: &[(PathData, f64)], gaps: &[(PathData, f64)]) -> Result<Option<NodeId>> {
    let old = check_layers(&s.doc()?.doc);
    if old.is_empty() && thin.is_empty() && gaps.is_empty() {
        return Ok(None);
    }
    let id = s.edit("Check Line Weight", |d, sel| {
        if !old.is_empty() {
            d.remove_many(&old)?;
            sel.prune(d);
        }
        if thin.is_empty() && gaps.is_empty() {
            return Ok(None);
        }
        let mut groups = vec![];
        for (regions, color, group, one) in [(thin, thin_color(), "Thin parts", "Thin part"), (gaps, gap_color(), "Narrow gaps", "Narrow gap")] {
            if regions.is_empty() {
                continue;
            }
            let children = regions.iter().map(|(p, _)| mark_path(d, p, color, one)).collect();
            let mut g = Node::group(d.alloc_id(), children);
            g.name = Some(group.into());
            groups.push(Arc::new(g));
        }
        // Magenta (preset 5) in the Layers panel, like the marks.
        let mut layer = Node::layer(d.alloc_id(), LAYER_NAME, LayerColor::Preset(5));
        layer.locked = true;
        if let NodeKind::Layer { template, printable, children, .. } = &mut layer.kind {
            // A template layer never prints and exports leave it out.
            (*template, *printable, *children) = (true, false, groups);
        }
        Ok(Some(d.insert(None, usize::MAX, layer)?))
    })?;
    keep_a_current_layer(s)?;
    Ok(id)
}

/// After the check layer went, make sure the current layer is still one.
fn keep_a_current_layer(s: &mut Session) -> Result<()> {
    let st = s.doc_mut()?;
    st.layer_rows.retain(|r| st.doc.node(*r).is_some());
    if st.current_layer().is_none() {
        st.active_layer = st.doc.default_layer();
    }
    Ok(())
}

fn clear(s: &mut Session, _: &Value) -> Result<Value> {
    let d = &s.doc()?.doc;
    let old = check_layers(d);
    if old.is_empty() {
        return Ok(json!({ "removed": 0 }));
    }
    if old.len() == d.layers.len() {
        return Err(bad("lineweight.clear", "the check layer is the document's only layer, and a document needs one"));
    }
    s.edit("Clear Line Weight Check", |d, sel| {
        d.remove_many(&old)?;
        sel.prune(d);
        Ok(())
    })?;
    keep_a_current_layer(s)?;
    Ok(json!({ "removed": old.len() }))
}
