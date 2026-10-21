//! PAL geometry uses the same fallible curve operations as Offset Path and Pathfinder.
use super::super::pathops;
use super::{Action, Element, Player, Result, actions, error, flag, number, required, selection};
use serde_json::json;
use std::io::Write;
use vectorcraft_color::{Color, Paint, Swatch};
use vectorcraft_doc::{Appearance, Document, Node, NodeKind};
use vectorcraft_geom::{FillRule, PathData};
use vectorcraft_pathops::{self as po, BoolOp, Join};

fn combine(a: &PathData, b: &PathData, op: BoolOp) -> Result<PathData> {
    po::try_boolean(a, FillRule::NonZero, b, FillRule::NonZero, op, po::DEFAULT_PRECISION).map_err(error)
}

fn paint_stack(paths: &[(PathData, bool)]) -> Result<PathData> {
    // A solid rectangle that contains every input is already their union. This is exact,
    // not a bounds approximation: reject curves, holes, diagonal edges and knockout inputs.
    // It also avoids sweeping coincident copies of previously generated white footprints.
    if paths.iter().all(|(p, add)| *add || p.is_empty()) {
        let bounds = paths.iter().fold(None, |bounds, (p, _)| vectorcraft_geom::union_opt(bounds, p.bounds()));
        if let Some(bounds) = bounds {
            for (p, _) in paths {
                if solid_rectangle(p).is_some_and(|r| r.contains_rect(bounds)) {
                    return Ok(p.clone());
                }
            }
        }
    }
    po::try_paint_stack_with_control(
        &paths.iter().map(|(p, chosen)| (p, FillRule::NonZero, *chosen)).collect::<Vec<_>>(),
        po::GeometryBudget::default(),
        vectorcraft_doc::cancel::is_cancelled,
    )
    .map_err(error)
}

fn solid_rectangle(path: &PathData) -> Option<vectorcraft_geom::Rect> {
    let [sp] = path.subpaths.as_slice() else { return None };
    let [a, b, c, d] = sp.anchors.as_slice() else { return None };
    if !sp.closed || sp.anchors.iter().any(|a| a.p != a.h_in || a.p != a.h_out || !a.p.x.is_finite() || !a.p.y.is_finite()) {
        return None;
    }
    let axis_aligned = (a.p.x == b.p.x && b.p.y == c.p.y && c.p.x == d.p.x && d.p.y == a.p.y)
        || (a.p.y == b.p.y && b.p.x == c.p.x && c.p.y == d.p.y && d.p.x == a.p.x);
    if !axis_aligned || a.p.x == c.p.x || a.p.y == c.p.y {
        return None;
    }
    Some(vectorcraft_geom::Rect::from_points(a.p, c.p))
}

fn offset_options(p: &Element, choke: bool) -> Result<(f64, Join, f64)> {
    let distance = number(p, "Distance")?;
    if !(0.0..=10_000.0).contains(&distance) {
        return Err(error("PAL distance must be between 0 and 10000 mm"));
    }
    let join = match required(p, "JoinType")? {
        "Round" => Join::Round,
        "Miter" => Join::Miter,
        "Bevel" => Join::Bevel,
        v => return Err(error(format!("unsupported JoinType {v}"))),
    };
    let miter = if p.value("MiterLimit").is_some() { number(p, "MiterLimit")? } else { 4.0 };
    if !(1.0..=500.0).contains(&miter) {
        return Err(error("PAL miter limit must be between 1 and 500"));
    }
    Ok((distance * 72.0 / 25.4 * if choke { -1.0 } else { 1.0 }, join, miter))
}
fn offset(path: &PathData, options: (f64, Join, f64)) -> Result<PathData> {
    if options.0 == 0.0 {
        return Ok(path.clone());
    }
    po::try_offset_path(path, options.0, options.1, options.2).map_err(error)
}

/// Painted footprint, including outlined strokes and real clip intersections, never bounds.
fn footprint(d: &mut Document, n: &Node, brushes: &[vectorcraft_brush::Brush], covered: Option<&PathData>) -> Result<PathData> {
    if !n.visible || n.opacity == 0.0 {
        return Ok(PathData::default());
    }
    if let Some(reason) = composition_issue(d, n, n, covered) {
        return Err(error(format!("PAL vector geometry: {reason} on object {}", n.id)));
    }
    if matches!(n.kind, NodeKind::Path { clipping: true, .. } | NodeKind::Path { guide: true, .. }) {
        return Ok(PathData::default());
    }
    if let NodeKind::Mesh(mesh) = &n.kind {
        if mesh.points.iter().any(|p| p.opacity != 1.0) {
            return Err(error("PAL vector geometry requires opaque artwork without masks or effects"));
        }
        return po::try_normalize(&mesh.outline(), FillRule::NonZero).map_err(error);
    }
    if let Some(out) = pathops::outline_strokes(d, brushes, n) {
        return footprint(d, &out, brushes, covered);
    }
    if let Some((path, rule)) = pathops::node_path(n) {
        return if n.appearance.fill_paint().is_none() { Ok(PathData::default()) } else { po::try_normalize(&path, rule).map_err(error) };
    }
    if let Some(cs) = n.children() {
        let mut path = PathData::default();
        for c in cs.iter().skip(usize::from(n.clips())) {
            path = combine(&path, &footprint(d, c, brushes, covered)?, BoolOp::Union)?;
        }
        if n.clips()
            && let Some(mask) = cs.first()
        {
            let (clip, rule) = pathops::node_path(mask).ok_or_else(|| error("PAL clip mask must be a path"))?;
            path = po::try_boolean(&path, FillRule::NonZero, &clip, rule, BoolOp::Intersect, po::DEFAULT_PRECISION).map_err(error)?;
        }
        return Ok(path);
    }
    Err(error("PAL vector geometry cannot outline this object type"))
}

fn composition_issue(d: &Document, n: &Node, content: &Node, covered: Option<&PathData>) -> Option<String> {
    if n.opacity != 1.0 {
        return Some(format!("unsupported opacity {}", n.opacity));
    }
    if n.blend != vectorcraft_color::BlendMode::Normal {
        return Some(format!("unsupported blend {:?}", n.blend));
    }
    if n.knockout == vectorcraft_doc::Knockout::On {
        return Some(format!("unsupported knockout {:?}", n.knockout));
    }
    if let Some(mask) = &n.extras().mask
        && !mask_identity(d, mask, content, covered)
    {
        return Some("opacity mask does not establish full coverage over printed content (partial, zero, or unsupported mask composition)".into());
    }
    if n.appearance.effects.iter().any(|e| !e.is_noop()) {
        return Some("unsupported appearance effects".into());
    }
    for item in &n.appearance.items {
        let (visible, opacity, blend, effects) = match item {
            vectorcraft_doc::AppearanceItem::Fill(f) => (f.visible, f.opacity, f.blend, f.effects.iter().any(|e| !e.is_noop())),
            vectorcraft_doc::AppearanceItem::Stroke(s) => (s.visible, s.opacity, s.blend, s.effects.iter().any(|e| !e.is_noop())),
        };
        if visible && (opacity != 1.0 || blend != vectorcraft_color::BlendMode::Normal || effects) {
            return Some(format!("unsupported paint composition (opacity {opacity}, blend {blend:?}, effects {effects})"));
        }
    }
    None
}

/// Prove identity geometrically, without a raster sampling tolerance that could miss a hole.
/// Imported alpha masks already have white art; luminosity masks keep their actual colours.
fn mask_identity(d: &Document, mask: &vectorcraft_doc::OpacityMask, content: &Node, covered: Option<&PathData>) -> bool {
    if mask.disabled {
        return true;
    }
    let Some(bounds) = content.visual_bounds() else { return true };
    let mut target = PathData::from_bezpath(&kurbo::Shape::to_path(&bounds, 0.01));
    if let NodeKind::Mesh(mesh) = &content.kind {
        target = mesh.outline();
    } else if !content
        .appearance
        .items
        .iter()
        .any(|item| matches!(item, vectorcraft_doc::AppearanceItem::Stroke(s) if s.visible && s.opacity != 0.0 && s.paint != Paint::None))
        && let Some((path, rule)) = pathops::node_path(content)
    {
        let Ok(path) = po::try_normalize(&path, rule) else { return false };
        target = path;
    }
    // A mask only matters where this object survives its clipping ancestors.
    for id in d.ancestry(content.id).unwrap_or_default() {
        if let Some(parent) = d.node(id).filter(|n| n.clips()) {
            let Some((clip, rule)) = parent.children().and_then(|cs| cs.first()).and_then(|n| pathops::node_path(n)) else { return false };
            let Ok(clipped) = po::try_boolean(&target, FillRule::NonZero, &clip, rule, BoolOp::Intersect, po::DEFAULT_PRECISION) else {
                return false;
            };
            target = clipped;
        }
    }
    if target.is_empty() {
        return true;
    }
    // A selected contribution wholly inside the established underprint cannot change
    // its footprint, whatever coverage the mask removes. This is only supplied for
    // selected additive art, never for a knockout/cover or for ordinary path operations.
    if covered.is_some_and(|covered| combine(&target, covered, BoolOp::Difference).is_ok_and(|outside| outside.is_empty())) {
        return true;
    }
    let Ok((painted, opaque)) = mask_regions(d, &mask.art, mask.invert) else { return false };
    let covered = if mask.clip == mask.invert {
        combine(&target, &painted, BoolOp::Difference).and_then(|outside| combine(&outside, &opaque, BoolOp::Union))
    } else {
        Ok(opaque)
    };
    covered.and_then(|cover| combine(&target, &cover, BoolOp::Difference)).is_ok_and(|missing| missing.is_empty())
}

/// Painted region and region known to have the required opaque extreme (white, or black
/// for an inverted mask). Later paint replaces earlier coverage. Clips are applied exactly.
fn mask_regions(d: &Document, n: &Node, black: bool) -> Result<(PathData, PathData)> {
    let empty = || (PathData::default(), PathData::default());
    if !n.visible || n.opacity == 0.0 {
        return Ok(empty());
    }
    if n.opacity != 1.0
        || n.blend != vectorcraft_color::BlendMode::Normal
        || n.knockout == vectorcraft_doc::Knockout::On
        || n.extras().mask.as_ref().is_some_and(|m| !m.disabled)
        || n.appearance.effects.iter().any(|e| !e.is_noop())
    {
        return Err(error("mask composition cannot be proven opaque"));
    }
    let extreme = |c: Color| c.to_rgb() == [if black { 0.0 } else { 1.0 }; 3];
    if let NodeKind::Image(im) = &n.kind {
        let v = if black { 0 } else { 255 };
        let blob = d.images.get(&im.key).ok_or_else(|| error("missing mask image"))?;
        if blob.is_opaque() != Some(true) {
            return Err(error("mask image is not fully opaque"));
        }
        let path =
            PathData::from_bezpath(&kurbo::Shape::to_path(&vectorcraft_geom::Rect::new(0., 0., f64::from(im.width), f64::from(im.height)), 0.01))
                .transformed(im.xf);
        return Ok((path.clone(), if blob.all_pixels_equal([v, v, v, 255]) == Some(true) { path } else { PathData::default() }));
    }
    if let NodeKind::Mesh(mesh) = &n.kind {
        let path = po::try_normalize(&mesh.outline(), FillRule::NonZero).map_err(error)?;
        let opaque = mesh.points.iter().all(|p| p.opacity == 1.0 && extreme(p.color));
        return Ok((path.clone(), if opaque { path } else { PathData::default() }));
    }
    if let Some((path, rule)) = pathops::node_path(n) {
        let mut printed = false;
        let mut white = false;
        for item in &n.appearance.items {
            match item {
                vectorcraft_doc::AppearanceItem::Fill(f) if f.visible && f.opacity != 0.0 && f.paint != Paint::None => {
                    if f.opacity != 1.0 || f.blend != vectorcraft_color::BlendMode::Normal || f.effects.iter().any(|e| !e.is_noop()) {
                        return Err(error("mask fill cannot be proven opaque"));
                    }
                    printed = true;
                    white = match &f.paint {
                        Paint::Solid { color, swatch: None, .. } => extreme(*color),
                        Paint::Gradient(g) => {
                            !g.gradient.stops.is_empty()
                                && g.gradient.stops.iter().all(|s| s.opacity == 1.0 && s.swatch.is_none() && extreme(s.color))
                        }
                        _ => false,
                    };
                }
                vectorcraft_doc::AppearanceItem::Stroke(s) if s.visible && s.opacity != 0.0 && s.paint != Paint::None => {
                    return Err(error("mask stroke cannot be proven opaque"));
                }
                _ => {}
            }
        }
        if !printed {
            return Ok(empty());
        }
        let path = po::try_normalize(&path, rule).map_err(error)?;
        return Ok((path.clone(), if white { path } else { PathData::default() }));
    }
    if n.appearance.items.iter().any(|item| match item {
        vectorcraft_doc::AppearanceItem::Fill(f) => f.visible && f.opacity != 0.0 && f.paint != Paint::None,
        vectorcraft_doc::AppearanceItem::Stroke(s) => s.visible && s.opacity != 0.0 && s.paint != Paint::None,
    }) {
        return Err(error("mask container paint cannot be proven opaque"));
    }
    let children = n.children().ok_or_else(|| error("mask artwork cannot be proven opaque"))?;
    let (mut painted, mut opaque) = empty();
    for child in children.iter().skip(usize::from(n.clips())) {
        let (cover, white) = mask_regions(d, child, black)?;
        opaque = combine(&combine(&opaque, &cover, BoolOp::Difference)?, &white, BoolOp::Union)?;
        painted = combine(&painted, &cover, BoolOp::Union)?;
    }
    if n.clips() {
        let clip = children.first().and_then(|n| pathops::node_path(n)).ok_or_else(|| error("unsupported mask clip"))?;
        let clip = po::try_normalize(&clip.0, clip.1).map_err(error)?;
        painted = combine(&painted, &clip, BoolOp::Intersect)?;
        opaque = combine(&opaque, &clip, BoolOp::Intersect)?;
    }
    Ok((painted, opaque))
}

fn ancestor_prints(d: &Document, n: &Node, covered: Option<&PathData>) -> Result<bool> {
    // These paths define geometry but do not paint a footprint, so their ancestors'
    // transparency cannot affect the requested underprint.
    if matches!(n.kind, NodeKind::Path { clipping: true, .. } | NodeKind::Path { guide: true, .. }) {
        return Ok(false);
    }
    let ancestors = d.ancestry(n.id).unwrap_or_default();
    // Test zero opacity first: invisible compositions contribute no footprint.
    if ancestors
        .iter()
        .filter_map(|id| d.node(*id))
        .any(|p| !p.visible || p.opacity == 0.0 || matches!(p.kind, NodeKind::Layer { printable: false, .. }))
    {
        return Ok(false);
    }
    for parent in ancestors.iter().filter_map(|id| d.node(*id)) {
        if let Some(reason) = composition_issue(d, parent, n, covered) {
            return Err(error(format!("PAL underprint {reason} on object/ancestor {}", parent.id)));
        }
    }
    Ok(true)
}

fn clipped_footprint(d: &mut Document, n: &Node, brushes: &[vectorcraft_brush::Brush], covered: Option<&PathData>) -> Result<PathData> {
    if !ancestor_prints(d, n, covered)? {
        return Ok(PathData::default());
    }
    let mut path = footprint(d, n, brushes, covered)?;
    for id in d.ancestry(n.id).unwrap_or_default() {
        if let Some(parent) = d.node(id).filter(|parent| parent.id != n.id && parent.clips()) {
            let mask = parent.children().and_then(|cs| cs.first()).ok_or_else(|| error("PAL clip mask must be a path"))?;
            let (clip, rule) = pathops::node_path(mask).ok_or_else(|| error("PAL clip mask must be a path"))?;
            path = po::try_boolean(&path, FillRule::NonZero, &clip, rule, BoolOp::Intersect, po::DEFAULT_PRECISION).map_err(error)?;
        }
    }
    Ok(path)
}

impl Player<'_> {
    pub(super) fn remove_background(&mut self) -> Result<()> {
        // The explicitly requested house preparation rule, upstream of the action lists.
        let targets = actions::leaf_targets(self.doc()?, &selection::all_objects(self.doc()?, false));
        let targets = targets
            .into_iter()
            .filter_map(|id| self.doc().ok().and_then(|d| d.node(id).map(|n| (d, n))).map(|(d, n)| (id, selection::inks(d, n))))
            .collect::<Vec<_>>();
        let mut ids = Vec::new();
        for (id, inks) in targets {
            if inks?.get("SupaBG").is_some_and(|t| *t > 0.0) {
                ids.push(id);
            }
        }
        let count = ids.len();
        self.session.edit("Delete", |d, sel| {
            for id in ids {
                d.remove(id)?;
            }
            sel.clear();
            Ok(())
        })?;
        self.trace.push(
            json!({"list":"Preparation","position":0,"action":"DeleteBackground","params":"spot=SupaBG (house prep rule)","objectsAffected":count}),
        );
        writeln!(std::io::stderr(), "[Preparation #0] DeleteBackground spot=SupaBG (house prep rule) → {count} objects").map_err(error)
    }

    pub(super) fn geometry(&mut self, a: &Action) -> Result<usize> {
        let ids = self.ids()?;
        if ids.is_empty() {
            return Ok(0);
        }
        match a.kind.as_str() {
            "AddWhiteUnderPrint" => self.underprint(&a.params),
            "Choke" | "Spread" => self.resize_paths(&a.params, a.kind == "Choke"),
            "Shaper" => self.shaper(&a.params),
            other => Err(error(format!("not implemented for nonempty selection: {other}"))),
        }
    }

    fn underprint(&mut self, p: &Element) -> Result<usize> {
        let choke = match required(p, "TrappingMode")? {
            "Choke" => true,
            "Spread" => false,
            v => return Err(error(format!("unsupported TrappingMode {v}"))),
        };
        let options = offset_options(p, choke)?;
        let exclude = flag(p, "ExcludeCoveredAreas")?;
        let white = flag(p, "ExcludeWhiteObjects")?;
        if flag(p, "ExcludeDynamicMarks")? {
            return Err(error("PAL dynamic mark exclusion is not implemented"));
        }
        let ink = if flag(p, "UseSelectedSeparation")? {
            if self.inks.len() != 1 {
                return Err(error("white underprint requires exactly one selected separation"));
            }
            self.inks.first().cloned().ok_or_else(|| error("missing underprint separation"))?
        } else {
            "Opaque White".into()
        };
        let doc = self.doc()?;
        let selected: std::collections::BTreeSet<_> = actions::leaf_targets(doc, &self.ids()?).into_iter().collect();
        let all = actions::leaf_targets(doc, &selection::all_objects(doc, false));
        let cached: std::collections::BTreeMap<_, _> = self
            .trap_footprints
            .iter()
            .filter(|(id, (node, _))| doc.node(**id) == Some(node))
            .filter(|(id, _)| !selected.iter().any(|s| doc.ancestry(*s).is_some_and(|a| a.contains(id))))
            .map(|(id, (_, path))| (*id, path.clone()))
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        let all: Vec<_> = all
            .into_iter()
            .filter_map(|id| {
                let replacement = doc.ancestry(id).and_then(|parents| parents.into_iter().find(|p| cached.contains_key(p))).unwrap_or(id);
                seen.insert(replacement).then_some(replacement)
            })
            .collect();
        let brushes = vectorcraft_brush::library(self.doc()?);
        self.session.edit("Offset Path", |d, sel| {
            let started = std::time::Instant::now();
            // Precision cleanup after each boolean is not associative. Keep that established
            // operation order for small stacks; only detailed art needs the bulk sweep to
            // avoid repeatedly processing an ever-growing outline.
            let batched = all.len() > 1024;
            let mut region = PathData::default();
            let mut paths = Vec::new();
            let mut has_selected = false;
            let mut operations = 0;
            for id in &all {
                vectorcraft_doc::cancel::check().map_err(error)?;
                let mut n = d.node(*id).cloned().ok_or_else(|| error("PAL object disappeared"))?;
                // Hidden/nonprinting ancestors do not cover artwork. Locked art still does.
                if d.ancestry(*id).is_some_and(|parents| {
                    parents.iter().any(|i| d.node(*i).is_some_and(|n| !n.visible || matches!(n.kind, NodeKind::Layer { printable: false, .. })))
                }) {
                    continue;
                }
                let chosen = selected.contains(id) && (!white || selection::inks(d, &n)?.values().any(|t| *t > 0.0));
                if !chosen && (!exclude || if batched { !has_selected } else { region.is_empty() }) {
                    continue;
                }
                if !chosen {
                    // Overprinting ink does not knock out the selected artwork underneath.
                    // Only knockout fills/strokes participate in covered-area subtraction.
                    n.appearance.items.retain(|item| match item {
                        vectorcraft_doc::AppearanceItem::Fill(f) => !f.overprint,
                        vectorcraft_doc::AppearanceItem::Stroke(s) => !s.overprint,
                    });
                }
                let prior = if chosen
                    && batched
                    && d.ancestry(n.id).is_some_and(|ids| ids.iter().any(|id| d.node(*id).is_some_and(|n| n.extras().mask.is_some())))
                {
                    Some(paint_stack(&paths)?)
                } else {
                    None
                };
                let covered = chosen.then_some(prior.as_ref().unwrap_or(&region));
                if !ancestor_prints(d, &n, covered)? {
                    continue;
                }
                let path = match cached.get(id) {
                    Some(path) => path.clone(),
                    None => match clipped_footprint(d, &n, &brushes, covered) {
                        Ok(path) => path,
                        // As in the sequential fold, a cover after an empty region is irrelevant.
                        Err(_) if batched && !chosen && paint_stack(&paths)?.is_empty() => {
                            paths.clear();
                            has_selected = false;
                            continue;
                        }
                        Err(e) => return Err(e),
                    },
                };
                has_selected |= chosen && !path.is_empty();
                operations += 1;
                if batched {
                    paths.push((path, chosen));
                } else {
                    region = combine(&region, &path, if chosen { BoolOp::Union } else { BoolOp::Difference })?;
                }
            }
            let prepared = started.elapsed();
            if batched {
                region = paint_stack(&paths)?;
            }
            if std::env::var_os("VECTORCRAFT_SEP_PROFILE").is_some() {
                writeln!(
                    std::io::stderr(),
                    "[underprint] objects={} selected={} inputs={} batched={} anchors={} prepare_ms={} sweep_ms={}",
                    all.len(),
                    selected.len(),
                    operations,
                    batched,
                    paths.iter().flat_map(|(p, _)| &p.subpaths).map(|p| p.anchors.len()).sum::<usize>(),
                    prepared.as_millis(),
                    started.elapsed().saturating_sub(prepared).as_millis()
                )
                .map_err(error)?;
            }
            let region = offset(&region, options)?;
            sel.clear();
            if region.is_empty() {
                return Ok(0);
            }
            if ink == "Opaque White" && d.swatch(&ink).is_none() {
                d.swatches.push(Swatch { name: ink.clone(), paint: Paint::solid(Color::rgb(1.0, 1.0, 1.0)), global: true, spot: true });
            }
            let paint = actions::flat_paint(d, std::slice::from_ref(&ink), 1.0, false)?;
            let layer = selection::layer_ids(d)
                .into_iter()
                .find(|id| d.node(*id).is_some_and(|n| n.name.as_deref() == Some("White Underprint")))
                .unwrap_or_else(|| d.add_layer(Some("White Underprint")));
            let mut app = Appearance::default();
            app.set_fill(paint);
            app.set_stroke(Paint::None);
            let node = Node::path(d.alloc_id(), region, app);
            let id = node.id;
            d.insert(Some(layer), usize::MAX, node)?;
            sel.set([id]);
            Ok(1)
        })
    }

    fn resize_paths(&mut self, p: &Element, choke: bool) -> Result<usize> {
        let options = offset_options(p, choke)?;
        let target = required(p, "SpreadTarget")?;
        if !matches!(target, "OriginalPath" | "CopyInFront" | "CopyBehind") {
            return Err(error(format!("unsupported SpreadTarget {target}")));
        }
        let ids = actions::leaf_targets(self.doc()?, &self.ids()?);
        self.session.edit("Offset Path", |d, sel| {
            let mut results = Vec::new();
            for id in &ids {
                let n = d.node(*id).cloned().ok_or_else(|| error("PAL object disappeared"))?;
                let (path, rule) = pathops::node_path(&n).ok_or_else(|| error("PAL spread/choke requires paths"))?;
                let path = offset(&po::try_normalize(&path, rule).map_err(error)?, options)?;
                let (parent, at, _) = d.position(*id).ok_or_else(|| error("PAL object position disappeared"))?;
                if target == "OriginalPath" {
                    d.remove(*id)?;
                }
                if path.is_empty() {
                    continue;
                }
                let mut new = n.clone();
                if target != "OriginalPath" {
                    new.id = d.alloc_id();
                }
                match &mut new.kind {
                    NodeKind::Path { path: original, .. } => *original = path,
                    _ => new.kind = NodeKind::Path { path, rule: vectorcraft_geom::FillRule::NonZero, clipping: false, guide: false, live: None },
                }
                results.push(new.id);
                d.insert(parent, at + usize::from(target == "CopyInFront"), new)?;
            }
            let count = results.len();
            sel.set(results);
            Ok(count)
        })
    }

    fn shaper(&mut self, p: &Element) -> Result<usize> {
        let operation = required(p, "Operation")?;
        let op = match operation {
            "Unite" => BoolOp::Union,
            "Intersect" => BoolOp::Intersect,
            "Exclude" => BoolOp::Xor,
            "MinusFront" | "MinusBack" => BoolOp::Difference,
            v => return Err(error(format!("unsupported Shaper operation {v}"))),
        };
        let copy = flag(p, "WorkOnCopy")?;
        let selected = actions::roots(self.doc()?, &self.ids()?);
        let mut ids = selection::all_objects(self.doc()?, false).into_iter().filter(|id| selected.contains(id)).collect::<Vec<_>>();
        if operation == "MinusBack" {
            ids.reverse();
        }
        let brushes = vectorcraft_brush::library(self.doc()?);
        let active = Some(self.active_layer()?);
        self.session.edit("Pathfinder", |d, sel| {
            let mut result = None;
            for id in &ids {
                let n = d.node(*id).cloned().ok_or_else(|| error("PAL object disappeared"))?;
                let path = footprint(d, &n, &brushes, None)?;
                result = Some(match result {
                    None => path,
                    Some(ref r) => combine(r, &path, op)?,
                });
            }
            let path = result.unwrap_or_default();
            let style_id = if operation == "MinusFront" || operation == "MinusBack" { ids.first() } else { ids.last() };
            let style = style_id.and_then(|id| d.node(*id)).cloned().ok_or_else(|| error("PAL shaper has no source"))?;
            let leaves = actions::leaf_targets(d, &[style.id]);
            let mut appearance = leaves.last().and_then(|id| d.node(*id)).map(|n| (*n.appearance).clone()).unwrap_or_default();
            appearance.set_stroke(Paint::None);
            let node = Node::path(d.alloc_id(), path.clone(), appearance);
            let id = node.id;
            if copy {
                if !path.is_empty() {
                    d.insert(active, usize::MAX, node)?;
                }
            } else if path.is_empty() {
                d.remove_many(&selected)?;
            } else {
                d.wrap(&selected, |_| node)?;
            }
            sel.clear();
            if !path.is_empty() {
                sel.set([id]);
            }
            Ok(usize::from(!path.is_empty()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_geom::{Point, Rect, shapes};

    #[test]
    fn containing_rectangle_union_requires_solid_geometry_and_additive_inputs() {
        let outer = shapes::rectangle(Rect::new(0., 0., 20., 20.));
        let inner = shapes::rectangle(Rect::new(5., 5., 15., 15.));
        assert_eq!(paint_stack(&[(inner.clone(), true), (outer.clone(), true)]).unwrap(), outer);
        let cut = paint_stack(&[(outer.clone(), true), (inner.clone(), false)]).unwrap();
        assert!((po::area(&cut, FillRule::NonZero) - 300.).abs() < 1e-6);
        assert!(solid_rectangle(&cut).is_none(), "a rectangular outline with a hole is not solid");
        let mut curved = outer.clone();
        curved.subpaths[0].anchors[0].h_out = Point::new(3., 2.);
        assert!(solid_rectangle(&curved).is_none());
        let mut bowtie = outer.clone();
        bowtie.subpaths[0].anchors.swap(1, 2);
        assert!(solid_rectangle(&bowtie).is_none());
        let beyond = shapes::rectangle(Rect::new(19., 5., 25., 15.));
        let joined = paint_stack(&[(outer, true), (beyond, true)]).unwrap();
        assert!((po::area(&joined, FillRule::NonZero) - 450.).abs() < 1e-6);
    }
}
