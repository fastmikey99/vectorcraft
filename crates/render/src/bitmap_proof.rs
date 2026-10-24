//! Placed-image Separations Preview. Process samples stay process samples; a
//! CMYK image must never be separated again from its display RGB preview.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use vectorcraft_color::cms::{self, Cms, CmykLut, PROCESS_PLATES, ProofLut, ProofSetup};
use vectorcraft_doc::{Document, ImageBlob};
use vello_cpu::Pixmap;

use crate::images::{self, ImageKey};

struct Tables {
    owner: Arc<Cms>,
    profiles: u64,
    proof: ProofSetup,
    generation: u64,
    single: Option<Option<usize>>,
    cmyk: Option<CmykLut>,
    rgb: ProofLut,
}

static TABLES: Mutex<Option<Arc<Tables>>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(1);

fn tables(proof: &ProofSetup) -> Arc<Tables> {
    let owner = cms::active();
    let profiles = cms::profiles_generation();
    let mut held = TABLES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = &*held
        && Arc::ptr_eq(&owner, &t.owner)
        && profiles == t.profiles
        && proof == &t.proof
    {
        return t.clone();
    }
    let visible = proof.separations.as_deref().unwrap_or_default();
    let single = (visible.len() == 1).then(|| PROCESS_PLATES.iter().position(|p| *p == visible[0]));
    let selected = PROCESS_PLATES.map(|p| visible.iter().any(|v| v == p));
    let show = |inks: [f32; 4]| match single {
        Some(index) => [1.0 - index.map_or(0.0, |i| inks[i]); 3],
        None => owner.proof_cmyk_to_srgb(std::array::from_fn(|i| if selected[i] { inks[i] } else { 0.0 }), proof),
    };
    let cmyk = single.is_none().then(|| {
        CmykLut::build(|inks| {
            let [r, g, b] = show(inks);
            [r, g, b, 0.0]
        })
    });
    let rgb = ProofLut::build(|rgb| show(owner.srgb_to_cmyk(rgb, proof.intent)));
    let t = Arc::new(Tables { owner, profiles, proof: proof.clone(), generation: GENERATION.fetch_add(1, Ordering::Relaxed), single, cmyk, rgb });
    *held = Some(t.clone());
    t
}

/// Preview pixels share the ordinary bounded image store, including mipmaps.
/// Adjusted images use their adjusted RGB; unadjusted CMYK uses the original inks, and its spot
/// channels print on their own plates (in their swatches' colours in `doc` with several plates).
pub(crate) fn image(
    doc: &Document,
    key: &ImageKey,
    pm: &Arc<Pixmap>,
    blob: Option<&ImageBlob>,
    proof: &ProofSetup,
    touch: bool,
) -> Option<(ImageKey, Arc<Pixmap>)> {
    let t = tables(proof);
    let want = ImageKey { separation: t.generation, ..key.clone() };
    if let Some(hit) = crate::lookup(&want, touch) {
        return Some((want, hit));
    }
    let sized = |w: u32, h: u32| w == u32::from(pm.width()) && h == u32::from(pm.height());
    let own = blob.filter(|_| key.luts.0 != 0 && key.adjust.is_none() && !key.grey);
    let inks = own.and_then(ImageBlob::cmyk).filter(|i| sized(i.width, i.height));
    let samples = inks.as_ref().map(|i| i.data.as_chunks::<4>().0);
    let spots = match inks.as_ref().and(own).filter(|_| key.spots != 0) {
        Some(blob) => crate::paint::decode_spots(blob)?.filter(|s| sized(s.width, s.height)),
        None => None,
    };
    let visible = proof.separations.as_deref().unwrap_or_default();
    // The spot channel the one visible plate is, or for several plates each visible spot channel's
    // display colour.
    let (single_spot, shown) = match &spots {
        Some(s) if visible.len() == 1 => (s.names.iter().position(|n| *n == visible[0]), vec![]),
        Some(s) => {
            let c = cms::active();
            let shown = s
                .names
                .iter()
                .map(|n| visible.iter().any(|v| v == n).then(|| crate::proof::spot_color(doc, n)).flatten().map(|col| c.display_rgb(&col)))
                .collect();
            (None, shown)
        }
        None => (None, vec![]),
    };
    let per = spots.as_ref().map_or(0, |s| s.names.len());
    let mut out = (**pm).clone();
    for (index, px) in out.data_mut().iter_mut().enumerate() {
        if px.a == 0 {
            continue;
        }
        let tints = spots.as_ref().and_then(|s| s.data.get(index * per..(index + 1) * per));
        let rgb = if let Some(ink) = samples.and_then(|s| s.get(index)) {
            match (t.single, single_spot) {
                (_, Some(i)) => [255 - tints.and_then(|t| t.get(i)).copied().unwrap_or(0); 3],
                (Some(channel), None) => [255 - channel.map_or(0, |c| ink[c]); 3],
                (None, None) => {
                    let mut rgb = t.cmyk.as_ref().map_or([1.0; 3], |lut| {
                        let [r, g, b, _] = lut.apply(ink.map(|v| f32::from(v) / 255.0));
                        [r, g, b]
                    });
                    for (v, s) in tints.unwrap_or_default().iter().zip(&shown) {
                        if let (Some(s), true) = (s, *v > 0) {
                            let v = f32::from(*v) / 255.0;
                            for i in 0..3 {
                                rgb[i] *= 1.0 - v * (1.0 - s[i].clamp(0.0, 1.0));
                            }
                        }
                    }
                    rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                }
            }
        } else {
            let straight = [px.r, px.g, px.b].map(|v| ((u32::from(v) * 255 + u32::from(px.a) / 2) / u32::from(px.a)).min(255) as u8);
            t.rgb.apply8(straight)
        };
        let [r, g, b] = rgb.map(|v| ((u32::from(v) * u32::from(px.a) + 127) / 255) as u8);
        (px.r, px.g, px.b) = (r, g, b);
    }
    let out = images::store().insert(want.clone(), Arc::new(out));
    Some((want, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RenderOptions, Renderer};
    use vectorcraft_doc::{Document, ImageObject, Node, NodeKind, OpacityMask};
    use vectorcraft_geom::Affine;

    fn picture(d: &mut Document, key: &str) -> Node {
        Node::new(
            d.alloc_id(),
            NodeKind::Image(Box::new(ImageObject {
                key: key.into(),
                width: 4,
                height: 4,
                xf: Affine::IDENTITY,
                link: None,
                placement: Default::default(),
            })),
        )
    }

    fn opts(plates: &[&str]) -> RenderOptions {
        RenderOptions {
            background: Some([255; 4]),
            smooth_images: false,
            proof: Some(ProofSetup { separations: Some(plates.iter().map(|s| (*s).into()).collect()), ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn bitmap_plate_selection_keeps_process_samples_and_invalidates_cached_pixels() {
        let mut d = Document::new(4.0, 4.0);
        let inks = vectorcraft_doc::cmyk::Inks::new(4, 4, [17, 129, 255, 0].repeat(16)).unwrap();
        d.images.insert("ink".into(), ImageBlob::cmyk_tiff(&inks).unwrap());
        let n = picture(&mut d, "ink");
        d.insert(Some(d.layers[0].id), 0, n).unwrap();
        let mut r = Renderer::new();
        for (plate, coverage) in [("Cyan", 17), ("Magenta", 129), ("Yellow", 255), ("Black", 0), ("WHP", 0), ("Cyan", 17)] {
            let frame = r.render_region_with(&d, d.artboards[0].rect, 1.0, &opts(&[plate]));
            assert_eq!(frame.pixel(2, 2), [255 - coverage, 255 - coverage, 255 - coverage, 255], "{plate}");
        }
        assert_eq!(d.images["ink"].cmyk().unwrap(), inks);
        assert_eq!(r.render_region_with(&d, d.artboards[0].rect, 1.0, &opts(&[])).pixel(2, 2), [255; 4]);
    }

    #[test]
    fn bitmap_opacity_mask_keeps_its_luminance_when_selecting_a_plate() {
        let mut d = Document::new(4.0, 4.0);
        let inks = vectorcraft_doc::cmyk::Inks::new(4, 4, [255, 0, 0, 0].repeat(16)).unwrap();
        d.images.insert("ink".into(), ImageBlob::cmyk_tiff(&inks).unwrap());
        let mut mask = image::RgbaImage::from_pixel(4, 4, image::Rgba([128, 128, 128, 255]));
        mask.put_pixel(0, 0, image::Rgba([0, 0, 0, 255]));
        let mut png = std::io::Cursor::new(vec![]);
        mask.write_to(&mut png, image::ImageFormat::Png).unwrap();
        d.images.insert("mask".into(), ImageBlob::new("image/png", png.into_inner()));
        let mask = picture(&mut d, "mask");
        let mut n = picture(&mut d, "ink");
        n.extras_mut().mask = Some(Box::new(OpacityMask::new(mask, true)));
        d.insert(Some(d.layers[0].id), 0, n).unwrap();
        let got = Renderer::new().render_region_with(&d, d.artboards[0].rect, 1.0, &opts(&["Cyan"]));
        assert_eq!(got.pixel(0, 0), [255; 4]);
        assert!(got.pixel(2, 2)[0].abs_diff(127) <= 1, "{:?}", got.pixel(2, 2));
    }

    /// A CMYK image's spot channels print on their spot plates, show in their swatch's colour,
    /// and leave the process plates alone.
    #[test]
    fn spot_channels_print_on_their_plates() {
        use vectorcraft_color::{Color, Paint, Swatch};
        use vectorcraft_doc::cmyk::{Inks, SpotInks};
        let mut d = Document::new(4.0, 4.0);
        d.swatches.push(Swatch { name: "Spot A".into(), paint: Paint::solid(Color::cmyk(0.0, 1.0, 0.0, 0.0)), global: true, spot: true });
        let inks = Inks::new(4, 4, [40, 0, 0, 0].repeat(16)).unwrap();
        let spots = SpotInks::new(4, 4, vec!["Spot A".into()], vec![200; 16]).unwrap();
        d.images.insert("ink".into(), ImageBlob::cmyk_spot_tiff(&inks, &spots).unwrap());
        let n = picture(&mut d, "ink");
        d.insert(Some(d.layers[0].id), 0, n).unwrap();
        let mut r = Renderer::new();
        for (plate, coverage) in [("Spot A", 200), ("Cyan", 40), ("Magenta", 0), ("Spot B", 0)] {
            let frame = r.render_region_with(&d, d.artboards[0].rect, 1.0, &opts(&[plate]));
            assert_eq!(frame.pixel(2, 2), [255 - coverage, 255 - coverage, 255 - coverage, 255], "{plate}");
        }
        // Shown with every plate, and on screen, the spot ink darkens the image's green channel
        // (the swatch is magenta) beyond what its cyan does.
        let cyan_only = {
            let mut e = Document::new(4.0, 4.0);
            e.images.insert("ink".into(), ImageBlob::cmyk_tiff(&inks).unwrap());
            let n = picture(&mut e, "ink");
            e.insert(Some(e.layers[0].id), 0, n).unwrap();
            Renderer::new()
                .render_region_with(
                    &e,
                    e.artboards[0].rect,
                    1.0,
                    &RenderOptions { background: Some([255; 4]), smooth_images: false, ..Default::default() },
                )
                .pixel(2, 2)
        };
        let shown = r
            .render_region_with(
                &d,
                d.artboards[0].rect,
                1.0,
                &RenderOptions { background: Some([255; 4]), smooth_images: false, ..Default::default() },
            )
            .pixel(2, 2);
        assert!(shown[1] + 60 < cyan_only[1], "{shown:?} vs {cyan_only:?}");
        let all = r.render_region_with(&d, d.artboards[0].rect, 1.0, &opts(&["Cyan", "Magenta", "Yellow", "Black", "Spot A"])).pixel(2, 2);
        assert!(all[1] + 60 < cyan_only[1], "{all:?} vs {cyan_only:?}");
    }
}
