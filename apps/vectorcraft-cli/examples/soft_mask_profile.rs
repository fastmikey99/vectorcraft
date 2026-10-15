//! Local-file profiling; prints timings/counts only, never embeds or exports input artwork.
use serde_json::json;
use std::{sync::Arc, time::Instant};
use vectorcraft_doc::{Document, Node, NodeKind};
use vectorcraft_geom::Affine;
use vectorcraft_mcp::{Backend, Headless};
use vectorcraft_render::{RenderOptions, Renderer};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
fn strip(n: &mut Arc<Node>, mode: &str) {
    let n = Arc::make_mut(n);
    if mode == "no-masks" {
        n.extras_mut().mask = None;
    }
    if mode == "no-images" && matches!(n.kind, NodeKind::Image(_)) {
        n.visible = false;
    }
    if let Some(c) = n.children_mut() {
        for n in c {
            strip(n, mode);
        }
    }
}
fn frame(r: &mut Renderer, d: &Document, w: u32, h: u32, v: Affine, o: &RenderOptions) -> f64 {
    let t = Instant::now();
    let img = std::hint::black_box(r.render(d, w, h, v, o));
    let elapsed = ms(t);
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    img.pixels.hash(&mut hash);
    println!("pixels {:016x}", hash.finish());
    elapsed
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let file = args.get(1).ok_or("FILE [width height threads mode]")?;
    let w = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(1440);
    let h = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(900);
    let threads = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(0);
    let mode = args.get(5).map(String::as_str).unwrap_or("normal");
    vectorcraft_render::set_image_cache_budget(256 << 20);
    let mut hl = Headless::new();
    let t = Instant::now();
    hl.call("app.open", json!({"path": file}))?;
    println!("open_ms {:.3}", ms(t));
    let mut doc = hl.session.doc()?.doc.as_ref().clone();
    let (mut nodes, mut masks, mut images, mut groups) = (0, 0, 0, 0);
    let mut target = None;
    let mut target_area = 0.0;
    for n in &doc.layers {
        n.walk(&mut |n| {
            nodes += 1;
            masks += usize::from(n.extras().mask.is_some());
            images += usize::from(matches!(n.kind, NodeKind::Image(_)));
            groups += usize::from(n.is_container());
            if n.extras().mask.is_some() && doc.is_editable(n.id) {
                let area = n.visual_bounds().map_or(0.0, |b| b.width() * b.height());
                if area > target_area {
                    target = Some(n.id);
                    target_area = area;
                }
            }
        });
    }
    println!("nodes {nodes} masks {masks} images {images} containers {groups}");
    for n in &mut doc.layers {
        strip(n, mode);
    }
    let b = doc.artboards.first().ok_or("no artboard")?.rect;
    let z = (w as f64 / b.width()).min(h as f64 / b.height()) * 0.95;
    let view = Affine::translate((w as f64 / 2., h as f64 / 2.)) * Affine::scale(z) * Affine::translate(-b.center().to_vec2());
    let mut mask_area = 0.0;
    for n in &doc.layers {
        n.walk(&mut |n| {
            if let Some(m) = n.extras().mask.as_ref()
                && let Some(b) = m.art.visual_bounds()
            {
                let r = view.transform_rect_bbox(b).inflate(2.0, 2.0).intersect(vectorcraft_geom::Rect::new(0.0, 0.0, w as f64, h as f64));
                mask_area += r.width().max(0.0) * r.height().max(0.0);
            }
        });
    }
    println!("mask_bounding_pixels {mask_area:.0} full_mask_pixels {}", masks as u64 * w as u64 * h as u64);
    let opts = RenderOptions { image_mips: true, progressive_placed: true, trace_views: true, ..Default::default() };
    let mut r = Renderer::new();
    r.threads = threads;
    println!("size {w}x{h} threads {threads} mode {mode} cold_ms {:.3}", frame(&mut r, &doc, w, h, view, &opts));
    for scenario in ["redraw", "pan", "zoom"] {
        let mut times = Vec::new();
        for i in 0..7 {
            let v = match scenario {
                "pan" => Affine::translate((i as f64 * 3., 0.)) * view,
                "zoom" => {
                    Affine::translate((w as f64 / 2., h as f64 / 2.))
                        * Affine::scale(1.0 + i as f64 * 0.025)
                        * Affine::translate((-(w as f64) / 2., -(h as f64) / 2.))
                        * view
                }
                _ => view,
            };
            times.push(frame(&mut r, &doc, w, h, v, &opts));
        }
        times.sort_by(f64::total_cmp);
        println!("{scenario} ms {times:?} drawn {} culled {}", r.stats.drawn, r.stats.culled);
    }
    if mode == "normal" {
        let id = target.ok_or("no editable masked object")?;
        let t = Instant::now();
        hl.call("engine.execute", json!({"command":"select.set", "params":{"ids":[id.0]}}))?;
        println!("select_ms {:.3} target_kind {}", ms(t), if doc.node(id).ok_or("missing target")?.is_container() { "container" } else { "leaf" });
        for _ in 0..7 {
            let t = Instant::now();
            hl.call("engine.execute", json!({"command":"object.move", "params":{"dx":20.0,"dy":0.0}}))?;
            let command = ms(t);
            let d = hl.session.doc()?.doc.clone();
            let draw = frame(&mut r, &d, w, h, view, &opts);
            println!("move_command_ms {command:.3} redraw_ms {draw:.3}");
        }
    }
    println!("image_cache_bytes {}", vectorcraft_render::image_cache_bytes());
    Ok(())
}
