//! Embedded raster resources in pattern cells. These are not selectable page objects and
//! must not be passed through the Links panel's selection/relink machinery. A resource is
//! shared by content key; any of its cell node ids addresses it, including after replacement.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::Arc;

use image::{ImageDecoder, ImageReader};
use serde_json::{Value, json};
use vectorcraft_color::Paint;
use vectorcraft_doc::{Document, ImageObject, Node, NodeKind};
use vectorcraft_geom::{Affine, Rect};

use super::*;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "links.patternImages", "Pattern Images", [], None,
            "{} embedded images in pattern definitions, grouped by shared pixels; any cell node id in ids addresses the resource across replacement and native save/open. Placements are tile image frames intersecting paint and clipping bounds (conservative for nonrectangular clips), in document points; useCount counts page objects using the resource, tileCount counts candidate tile placements; unused definitions have zero uses → {images: [{id, ids, patterns, pixelWidth, pixelHeight, colorSpace, hasAlpha, useCount, tileCount, placements: [{ownerId, transform, bounds, clipBounds, artboards}]}]}",
            has_doc, list),
        cmd!(query "links.exportPatternImage", "Export Pattern Image", [], None,
            "{id, path?} export an embedded pattern image at native resolution without rendering, transforms or colour conversion; path must have the stored format's extension. Without path returns bytes for a web host. Keeps the resource embedded → {id, name, mime, dataBase64?, path?}",
            has_doc, export),
        cmd!(
            "links.replacePatternImage",
            "Replace Pattern Image",
            [],
            None,
            "{id, path? (a PNG file), name?, dataBase64? (PNG bytes instead of path)} replace every pattern cell image sharing this resource, preserving each frame, clipping, opacity and tiling; pixel dimensions may change, file resolution is ignored. Keeps one shared embedded resource. One undo step → {id, updated, pixelWidth, pixelHeight}",
            has_doc,
            replace
        ),
    ]
}

fn resources(d: &Document) -> BTreeMap<String, Vec<(&str, &Node)>> {
    let mut out: BTreeMap<String, Vec<(&str, &Node)>> = BTreeMap::new();
    for p in d.patterns.iter() {
        for a in &p.art {
            a.walk(&mut |n| {
                if let NodeKind::Image(im) = &n.kind {
                    out.entry(im.key.clone()).or_default().push((&p.name, n));
                }
            });
        }
    }
    out
}

fn resource<'a>(d: &'a Document, p: &Value, cmd: &str) -> Result<(u64, &'a ImageObject)> {
    let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(cmd, "give a pattern image id from links.patternImages"))?;
    let mut found = None;
    for pat in d.patterns.iter() {
        for art in &pat.art {
            art.walk(&mut |n| {
                if n.id.0 == id
                    && let NodeKind::Image(im) = &n.kind
                {
                    found = Some(im.as_ref());
                }
            });
        }
    }
    found.map(|im| (id, im)).ok_or_else(|| bad(cmd, "no pattern image with that id"))
}

fn rect(r: Rect) -> Value {
    json!([r.x0, r.y0, r.x1, r.y1])
}

fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let r = a.intersect(b);
    (r.width() > 1e-7 && r.height() > 1e-7).then_some(r)
}

/// Walk painted pattern instances, carrying the accumulated pattern transform and clipping
/// bounds. Ordinary groups have no transform of their own: transforms live on their leaves.
struct Placements<'a> {
    doc: &'a Document,
    rows: BTreeMap<String, Vec<Value>>,
    budget: usize,
}

impl Placements<'_> {
    fn node(&mut self, n: &Node, xf: Affine, clip: Option<Rect>, owner: Option<u64>, depth: usize) -> Result<()> {
        if depth > 32 || self.budget == 0 {
            return Err(bad("links.patternImages", "pattern placement traversal limit exceeded"));
        }
        self.budget -= 1;
        let clip = if n.clips() {
            let Some(b) = n.children().and_then(|ch| ch.first()).and_then(|c| c.geometric_bounds()) else { return Ok(()) };
            let b = xf.transform_rect_bbox(b);
            match clip {
                Some(c) => intersection(c, b),
                None => Some(b),
            }
        } else {
            clip
        };
        if n.clips() && clip.is_none() {
            return Ok(());
        }
        if let (Some(owner), NodeKind::Image(im)) = (owner, &n.kind) {
            let transform = xf * im.xf * Affine::scale_non_uniform(im.width as f64, im.height as f64);
            let bounds = transform.transform_rect_bbox(Rect::new(0.0, 0.0, 1.0, 1.0));
            if let Some(visible) = clip.map_or(Some(bounds), |c| intersection(c, bounds)) {
                let artboards: Vec<usize> =
                    self.doc.artboards.iter().enumerate().filter(|(_, a)| intersection(a.rect, visible).is_some()).map(|(i, _)| i + 1).collect();
                self.rows.entry(im.key.clone()).or_default().push(json!({
                    "ownerId": owner, "transform": transform.as_coeffs(), "bounds": rect(bounds),
                    "clipBounds": rect(visible), "artboards": artboards
                }));
            }
        }
        if let Some(bounds) = n.visual_bounds() {
            let bounds = xf.transform_rect_bbox(bounds);
            if let Some(area) = clip.map_or(Some(bounds), |c| intersection(c, bounds)) {
                for item in &n.appearance.items {
                    if let Paint::Pattern { pattern, xf: paint_xf } = item.paint()
                        && let Some(def) = self.doc.pattern(pattern)
                    {
                        let matrix = xf * *paint_xf;
                        if !matrix.as_coeffs().iter().all(|v| v.is_finite()) || matrix.determinant().abs() < 1e-12 {
                            continue;
                        }
                        for offset in def.offsets_covering(matrix.inverse().transform_rect_bbox(area)) {
                            let matrix = matrix * def.instance_xf(offset);
                            for art in &def.art {
                                self.node(art, matrix, Some(area), Some(owner.unwrap_or(n.id.0)), depth + 1)?;
                            }
                        }
                    }
                }
            }
        }
        if let Some(children) = n.children() {
            for child in children {
                self.node(child, xf, clip, owner, depth + 1)?;
            }
        }
        Ok(())
    }
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let d = &s.doc()?.doc;
    let mut placements = Placements { doc: d, rows: BTreeMap::new(), budget: 1_000_000 };
    for layer in &d.layers {
        placements.node(layer, Affine::IDENTITY, None, None, 0)?;
    }
    let mut rows = vec![];
    for (key, nodes) in resources(d) {
        let Some((_, first)) = nodes.first() else { continue };
        let NodeKind::Image(im) = &first.kind else { continue };
        let mut ids: Vec<u64> = nodes.iter().map(|(_, n)| n.id.0).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut patterns: Vec<&str> = nodes.iter().map(|(name, _)| *name).collect();
        patterns.sort_unstable();
        patterns.dedup();
        let blob = d.images.get(&key).ok_or_else(|| bad("links.patternImages", "pattern image pixels are missing"))?;
        let decoder = ImageReader::new(Cursor::new(blob.bytes.as_ref()))
            .with_guessed_format()
            .map_err(|e| bad("links.patternImages", e.to_string()))?
            .into_decoder()
            .map_err(|e| bad("links.patternImages", e.to_string()))?;
        let color = decoder.color_type();
        let space = if blob.is_cmyk() {
            "CMYK"
        } else if color.channel_count() <= 2 {
            "Gray"
        } else {
            "RGB"
        };
        let uses = placements.rows.remove(&key).unwrap_or_default();
        let use_count = uses.iter().filter_map(|p| p["ownerId"].as_u64()).collect::<BTreeSet<_>>().len();
        rows.push(json!({ "id": ids.first(), "ids": ids, "patterns": patterns,
            "pixelWidth": im.width, "pixelHeight": im.height, "colorSpace": space,
            "hasAlpha": color.has_alpha(), "useCount": use_count, "tileCount": uses.len(), "placements": uses }));
    }
    rows.sort_by_key(|r| r["id"].as_u64());
    Ok(json!({ "images": rows }))
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "links.exportPatternImage";
    let d = &s.doc()?.doc;
    let (id, im) = resource(d, p, C)?;
    let blob = d.images.get(&im.key).filter(|b| !b.is_proxy()).ok_or_else(|| bad(C, "pattern image pixels are missing"))?;
    let ext = match blob.mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/tiff" => "tif",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => return Err(bad(C, "unsupported stored image format")),
    };
    let mut out = json!({ "id": id, "name": format!("pattern-image-{id}.{ext}"), "mime": blob.mime });
    if let Some(path) = str_param(p, "path") {
        let extension = fileio::extension(path);
        if extension != ext && !(ext == "jpg" && extension == "jpeg") && !(ext == "tif" && extension == "tiff") {
            return Err(bad(C, format!("use .{ext} to export the stored samples without conversion")));
        }
        fileio::write_file(path, &blob.bytes)?;
        out["path"] = json!(path);
    } else {
        out["dataBase64"] = json!(vectorcraft_format::base64_encode(&blob.bytes));
    }
    Ok(out)
}

fn replace_node(n: &mut Node, key: &str, new: &fileio::RasterImage) -> usize {
    let mut count = 0;
    if let NodeKind::Image(im) = &mut n.kind
        && im.key == key
    {
        im.xf *= Affine::scale_non_uniform(im.width as f64 / new.width as f64, im.height as f64 / new.height as f64);
        im.width = new.width;
        im.height = new.height;
        im.key = new.key.clone();
        im.link = None;
        count += 1;
    }
    if let Some(children) = n.children_mut() {
        for child in children {
            count += replace_node(Arc::make_mut(child), key, new);
        }
    }
    count
}

fn replace(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "links.replacePatternImage";
    let (id, im) = resource(&s.doc()?.doc, p, C)?;
    let key = im.key.clone();
    let source = fileio::source(p, C)?;
    if image::guess_format(&source.bytes).ok() != Some(image::ImageFormat::Png) {
        return Err(bad(C, "replacement must be a PNG"));
    }
    // Fully decode before changing the document: a plausible PNG header alone is not enough.
    let mut reader = ImageReader::new(Cursor::new(&source.bytes)).with_guessed_format().map_err(|e| bad(C, e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(fileio::MAX_RASTER_ALLOC);
    reader.limits(limits);
    reader.decode().map_err(|e| bad(C, e.to_string()))?;
    let new = fileio::raster_image(&source.bytes)?;
    let mut updated = 0;
    s.edit("Replace Pattern Image", |d, _| {
        d.images.insert(new.key.clone(), new.blob.clone());
        for pat in d.patterns.iter_mut() {
            for art in &mut pat.art {
                updated += replace_node(Arc::make_mut(art), &key, &new);
            }
        }
        Ok(())
    })?;
    Ok(json!({ "id": id, "updated": updated, "pixelWidth": new.width, "pixelHeight": new.height }))
}
