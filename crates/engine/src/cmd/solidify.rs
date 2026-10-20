//! Remove Soft Pixels (`object.image.solidifyAlpha`): every pixel of one embedded image made fully
//! opaque or fully clear, on the image's own stored samples.
//!
//! An image's transparency is stored in one of two places:
//!
//! - **Its own alpha channel**: a PNG (RGBA or grey + alpha, 8 or 16 bits a sample; a palette or a
//!   colour key reads as RGBA), a WebP or a GIF. The new image is a PNG of the same colour type
//!   and depth, keeping the source PNG's colour and resolution chunks (`iCCP`, `sRGB`, `gAMA`,
//!   `cHRM`, `cICP`, `pHYs`).
//! - **A greyscale opacity mask**: the image's opacity mask is one embedded greyscale image (8 or
//!   16 bits), which is how a PDF image with a soft mask is read when its colour samples are CMYK
//!   (the inks stay as they are, see the PDF importer's `alpha_mask`). The mask's image is the one
//!   thresholded (its `invert` honoured); the colour image isn't touched at all.
//!
//! An image with neither (a JPEG, a CMYK TIFF without a mask) has nothing to remove and is
//! refused. Alpha at or above `level` becomes opaque, below it clear. Colour samples are copied as
//! they are, never through premultiplied colour, so soft pixels keep their exact colour values.
//! The result is decoded again and checked against the source before it is kept: every alpha 0 or
//! full, equal to the thresholded source at every pixel, every colour sample unchanged.
//!
//! Images keyed by content share their pixels: the result goes in under a key of its own, and only
//! the images changed are pointed at it (with `copies`, the others showing the same pixels at the
//! same printed size), so copies at other sizes keep theirs. One undo step.

use std::io::Cursor;

use image::{DynamicImage, ImageBuffer};
use serde_json::{Value, json};
use vectorcraft_doc::{Document, ImageBlob, ImageObject, Node, NodeKind};

use super::*;

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "object.image.solidifyAlpha",
        "Remove Soft Pixels",
        [],
        None,
        "{id?: image (default: the one selected object), level?: 1–255 (128: alpha at or above it becomes opaque, below it clear), copies?: bool (default false; true: also every other image showing the same pixels and transparency at the same printed size)} every pixel of one embedded image fully opaque or fully clear: its own alpha channel, or the greyscale image of its opacity mask; colour samples and the pixel grid are kept exactly, the result is checked against the source before it is kept; one undo step → {id, ids (the images changed, in document order; empty when no pixel was soft), form: \"alpha\"|\"mask\", width, height (of the samples thresholded), level, softPixels (alpha 1–254 before), clearPixels, opaquePixels (after)}",
        has_doc,
        solidify
    )]
}

const C: &str = "object.image.solidifyAlpha";
const UNDO: &str = "Remove Soft Pixels";
/// The colour and resolution chunks of a source PNG that the new PNG keeps.
const KEPT_CHUNKS: [&[u8; 4]; 6] = [b"iCCP", b"sRGB", b"gAMA", b"cHRM", b"cICP", b"pHYs"];

/// Where an image's transparency is stored.
#[derive(Clone, Debug, PartialEq)]
enum Form {
    /// In its own pixels (blob `key`).
    Alpha,
    /// In the greyscale image `key` of its opacity mask, read inverted with `invert`.
    Mask { key: String, invert: bool },
}

/// The opacity mask in force on `n`: its image's key and `invert` when it is one embedded image;
/// `Err` when it is other art; `None` without one (or when it is disabled).
fn mask_image(n: &Node) -> Option<std::result::Result<(String, bool), ()>> {
    let m = n.extras().mask.as_deref().filter(|m| !m.disabled)?;
    Some(match &m.art.kind {
        NodeKind::Image(im) if im.link.is_none() && m.art.opacity >= 1.0 => Ok((im.key.clone(), m.invert)),
        _ => Err(()),
    })
}

/// The image `n` shows and where its transparency is stored (not yet checked to be readable).
fn form_of(n: &Node) -> Result<(&ImageObject, Form)> {
    let NodeKind::Image(im) = &n.kind else {
        return Err(EngineError::Other("select one image to remove its soft pixels".into()));
    };
    if im.link.is_some() {
        return Err(EngineError::Other("the image is linked: embed it to remove its soft pixels".into()));
    }
    let form = match mask_image(n) {
        None => Form::Alpha,
        Some(Ok((key, invert))) => Form::Mask { key, invert },
        Some(Err(())) => return Err(EngineError::Other("the image's opacity mask isn't one greyscale image".into())),
    };
    Ok((im, form))
}

/// An image's printed frame: its pixel width and height through its transform, in points.
fn frame(im: &ImageObject) -> (f64, f64) {
    let [a, b, c, d, _, _] = im.xf.as_coeffs();
    (a.hypot(b) * f64::from(im.width), c.hypot(d) * f64::from(im.height))
}

/// The images in the layers of `doc` showing the same pixels as image `id`, with the same
/// transparency, at its printed size (within a hundredth of a point), itself included, in document
/// order.
fn copies(doc: &Document, id: vectorcraft_doc::NodeId, form: &Form) -> Vec<vectorcraft_doc::NodeId> {
    let Some(NodeKind::Image(me)) = doc.node(id).map(|n| &n.kind) else { return vec![] };
    let (w, h) = frame(me);
    let mut out = vec![];
    doc.walk(|n| {
        if let NodeKind::Image(im) = &n.kind
            && im.key == me.key
            && (im.width, im.height) == (me.width, me.height)
            && im.link.is_none()
            && form_of(n).is_ok_and(|(_, f)| f == *form)
        {
            let (iw, ih) = frame(im);
            if (iw - w).abs() < 0.01 && (ih - h).abs() < 0.01 {
                out.push(n.id);
            }
        }
    });
    out
}

/// The pixels of encoded image `bytes`, at most [`vectorcraft_doc::MAX_RASTER_ALLOC`] of them.
fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    let unreadable = || EngineError::Other("the image's pixels can't be read".into());
    let mut r = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|_| unreadable())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(vectorcraft_doc::MAX_RASTER_ALLOC);
    r.limits(limits);
    r.decode().map_err(|_| unreadable())
}

/// What thresholding counted: alpha strictly between clear and full before, clear and full after.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    soft: u64,
    clear: u64,
    opaque: u64,
}

/// The samples of one image, `n` to a pixel, the alpha (or mask level) at index `a` of each.
struct Layout {
    n: usize,
    a: usize,
    /// The alpha read as `full − sample` (an inverted mask).
    invert: bool,
}

/// One sample type: its full value and the cut-off for `level` (on the 8-bit scale).
trait Sample: Copy + PartialEq + PartialOrd {
    const ZERO: Self;
    const FULL: Self;
    fn cut(level: u8) -> Self;
    fn flip(self) -> Self;
}

impl Sample for u8 {
    const ZERO: Self = 0;
    const FULL: Self = u8::MAX;
    fn cut(level: u8) -> Self {
        level
    }
    fn flip(self) -> Self {
        u8::MAX - self
    }
}

impl Sample for u16 {
    const ZERO: Self = 0;
    const FULL: Self = u16::MAX;
    // 8-bit level k is 16-bit k × 257 (255 → 65535).
    fn cut(level: u8) -> Self {
        u16::from(level) * 257
    }
    fn flip(self) -> Self {
        u16::MAX - self
    }
}

/// The stored sample of alpha `v` thresholded at `cut` (the sample itself is alpha, or its flip).
fn solid<T: Sample>(v: T, cut: T, invert: bool) -> T {
    let alpha = if invert { v.flip() } else { v };
    let out = if alpha >= cut { T::FULL } else { T::ZERO };
    if invert { out.flip() } else { out }
}

/// `src` with every alpha sample thresholded at `level`, and what was counted.
fn threshold<T: Sample>(src: &[T], l: &Layout, level: u8) -> (Vec<T>, Counts) {
    let cut = T::cut(level);
    let mut out = src.to_vec();
    let mut c = Counts::default();
    for px in out.chunks_exact_mut(l.n) {
        let Some(v) = px.get_mut(l.a) else { continue };
        let alpha = if l.invert { v.flip() } else { *v };
        if alpha != T::ZERO && alpha != T::FULL {
            c.soft += 1;
        }
        *v = solid(*v, cut, l.invert);
        if *v == if l.invert { T::FULL } else { T::ZERO } {
            c.clear += 1;
        } else {
            c.opaque += 1;
        }
    }
    (out, c)
}

/// Does `out` hold exactly `src` thresholded at `level`: the same number of samples, every colour
/// sample the same, every alpha clear or full and equal to the source's thresholded?
fn verified<T: Sample>(src: &[T], out: &[T], l: &Layout, level: u8) -> bool {
    let cut = T::cut(level);
    src.len() == out.len()
        && src.len().is_multiple_of(l.n)
        && src.chunks_exact(l.n).zip(out.chunks_exact(l.n)).all(|(s, o)| {
            s.iter().zip(o).enumerate().all(
                |(i, (s, o))| {
                    if i == l.a { (*o == T::ZERO || *o == T::FULL) && *o == solid(*s, cut, l.invert) } else { s == o }
                },
            )
        })
}

/// The new pixels of `img` thresholded at `level`, the counts, and the sample layout; `None` when
/// the image has no alpha (or, for a mask, isn't greyscale without alpha).
pub(crate) fn solidified(img: &DynamicImage, level: u8, mask: Option<bool>) -> Option<(DynamicImage, Counts)> {
    let (w, h) = (img.width(), img.height());
    macro_rules! run {
        ($buf:expr, $n:expr, $a:expr, $variant:ident) => {{
            let l = Layout { n: $n, a: $a, invert: mask.unwrap_or(false) };
            let (out, c) = threshold($buf.as_raw(), &l, level);
            Some((DynamicImage::$variant(ImageBuffer::from_raw(w, h, out)?), c))
        }};
    }
    match (img, mask) {
        (DynamicImage::ImageRgba8(b), None) => run!(b, 4, 3, ImageRgba8),
        (DynamicImage::ImageLumaA8(b), None) => run!(b, 2, 1, ImageLumaA8),
        (DynamicImage::ImageRgba16(b), None) => run!(b, 4, 3, ImageRgba16),
        (DynamicImage::ImageLumaA16(b), None) => run!(b, 2, 1, ImageLumaA16),
        (DynamicImage::ImageLuma8(b), Some(_)) => run!(b, 1, 0, ImageLuma8),
        (DynamicImage::ImageLuma16(b), Some(_)) => run!(b, 1, 0, ImageLuma16),
        _ => None,
    }
}

/// Is `out` exactly `src` thresholded at `level` (the same colour type, size and samples but for
/// alpha, which is clear or full and equal to the source's thresholded)?
pub(crate) fn check(src: &DynamicImage, out: &DynamicImage, level: u8, mask: Option<bool>) -> bool {
    let invert = mask.unwrap_or(false);
    if (src.width(), src.height()) != (out.width(), out.height()) {
        return false;
    }
    let l = |n, a| Layout { n, a, invert };
    match (src, out, mask) {
        (DynamicImage::ImageRgba8(s), DynamicImage::ImageRgba8(o), None) => verified(s.as_raw(), o.as_raw(), &l(4, 3), level),
        (DynamicImage::ImageLumaA8(s), DynamicImage::ImageLumaA8(o), None) => verified(s.as_raw(), o.as_raw(), &l(2, 1), level),
        (DynamicImage::ImageRgba16(s), DynamicImage::ImageRgba16(o), None) => verified(s.as_raw(), o.as_raw(), &l(4, 3), level),
        (DynamicImage::ImageLumaA16(s), DynamicImage::ImageLumaA16(o), None) => verified(s.as_raw(), o.as_raw(), &l(2, 1), level),
        (DynamicImage::ImageLuma8(s), DynamicImage::ImageLuma8(o), Some(_)) => verified(s.as_raw(), o.as_raw(), &l(1, 0), level),
        (DynamicImage::ImageLuma16(s), DynamicImage::ImageLuma16(o), Some(_)) => verified(s.as_raw(), o.as_raw(), &l(1, 0), level),
        _ => false,
    }
}

/// The chunks of PNG `bytes` (type, the whole chunk: length, type, data and CRC); `None` when they
/// aren't a well-formed PNG.
fn chunks(bytes: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    let mut rest = bytes.strip_prefix(b"\x89PNG\r\n\x1a\n")?;
    let mut out = vec![];
    while !rest.is_empty() {
        let len = usize::try_from(u32::from_be_bytes(rest.get(..4)?.try_into().ok()?)).ok()?;
        let end = len.checked_add(12)?;
        let kind: [u8; 4] = rest.get(4..8)?.try_into().ok()?;
        out.push((kind, rest.get(..end)?));
        rest = rest.get(end..)?;
    }
    Some(out)
}

/// PNG `png` with the colour and resolution chunks of PNG `source` ([`KEPT_CHUNKS`]) in place of
/// its own, just after its header; `png` as it is when `source` isn't a PNG or has none of them.
fn with_source_chunks(png: Vec<u8>, source: &[u8]) -> Vec<u8> {
    let Some(kept) = chunks(source).map(|c| c.into_iter().filter(|(k, _)| KEPT_CHUNKS.contains(&k)).collect::<Vec<_>>()) else { return png };
    if kept.is_empty() {
        return png;
    }
    let Some(own) = chunks(&png) else { return png };
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (k, c) in own {
        if KEPT_CHUNKS.contains(&&k) {
            continue;
        }
        out.extend_from_slice(c);
        if &k == b"IHDR" {
            for (_, c) in &kept {
                out.extend_from_slice(c);
            }
        }
    }
    out
}

/// The new image blob for the blob `source` thresholded at `level` (`mask`: it is a mask's image,
/// read inverted with `Some(true)`), checked by decoding it again → the blob, its size and the counts.
fn solidify_blob(source: &ImageBlob, level: u8, mask: Option<bool>) -> Result<(ImageBlob, (u32, u32), Counts)> {
    let none = || EngineError::Other("the image has no transparency: nothing to remove".into());
    if mask.is_none() && source.is_cmyk() {
        return Err(none());
    }
    let img = decode(&source.bytes)?;
    let (out, counts) = match solidified(&img, level, mask) {
        Some(v) => v,
        None if mask.is_some() => return Err(EngineError::Other("the image's opacity mask isn't one greyscale image".into())),
        None => return Err(none()),
    };
    let mut png = Vec::new();
    out.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|_| EngineError::Other("the image's pixels couldn't be written".into()))?;
    // Only the source is needed to check what was written: the copy goes before it is decoded.
    drop(out);
    let png = with_source_chunks(png, &source.bytes);
    let written = decode(&png)?;
    if !check(&img, &written, level, mask) {
        return Err(EngineError::Other("removing soft pixels failed its check: nothing was changed".into()));
    }
    Ok((ImageBlob::png(png), (img.width(), img.height()), counts))
}

/// Does decoded image `img` have alpha below full anywhere?
fn has_soft_or_clear_alpha(img: &DynamicImage) -> bool {
    match img {
        DynamicImage::ImageRgba8(b) => b.as_raw().as_chunks::<4>().0.iter().any(|p| p[3] != u8::MAX),
        DynamicImage::ImageLumaA8(b) => b.as_raw().as_chunks::<2>().0.iter().any(|p| p[1] != u8::MAX),
        DynamicImage::ImageRgba16(b) => b.as_raw().as_chunks::<4>().0.iter().any(|p| p[3] != u16::MAX),
        DynamicImage::ImageLumaA16(b) => b.as_raw().as_chunks::<2>().0.iter().any(|p| p[1] != u16::MAX),
        other => other.color().has_alpha(),
    }
}

fn solidify(s: &mut Session, p: &Value) -> Result<Value> {
    let level = match p.get("level") {
        None | Some(Value::Null) => 128,
        Some(v) => v
            .as_u64()
            .filter(|l| (1..=255).contains(l))
            .and_then(|l| u8::try_from(l).ok())
            .ok_or_else(|| bad(C, "level must be a whole number from 1 to 255"))?,
    };
    let all = match p.get("copies") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad(C, "copies must be true or false"))?,
    };
    let st = s.doc()?;
    let id = match id_param(p, "id") {
        Some(id) => id,
        None => match st.selection.objects.as_slice() {
            [id] => *id,
            _ => return Err(EngineError::Other("select one image to remove its soft pixels".into())),
        },
    };
    let node = st.doc.node(id).ok_or(EngineError::NoNode(id))?;
    let (im, form) = form_of(node)?;
    let blob_of = |key: &str| st.doc.images.get(key).filter(|b| !b.is_proxy());
    let unreadable = || EngineError::Other("the image's pixels can't be read".into());
    let colour = blob_of(&im.key).ok_or_else(unreadable)?;
    let (blob, size, counts) = match &form {
        Form::Alpha => solidify_blob(colour, level, None)?,
        Form::Mask { key, invert } => {
            // The colour image's own alpha would combine with the mask's: thresholding one of them
            // wouldn't leave every pixel opaque or clear.
            if !colour.is_cmyk() && !colour.bytes.starts_with(&[0xFF, 0xD8]) && has_soft_or_clear_alpha(&decode(&colour.bytes)?) {
                return Err(EngineError::Other("the image has its own transparency and an opacity mask: release the mask first".into()));
            }
            solidify_blob(blob_of(key).ok_or_else(unreadable)?, level, Some(*invert))?
        }
    };
    let ids = if counts.soft == 0 {
        vec![]
    } else if all {
        copies(&st.doc, id, &form)
    } else {
        vec![id]
    };
    if !ids.is_empty() {
        let key = blob.content_key();
        let changed = ids.clone();
        s.edit(UNDO, |d, _| {
            if !d.images.contains_key(&key) {
                d.images.insert(key.clone(), blob);
            }
            for id in &changed {
                let n = d.node_mut(*id).ok_or(EngineError::NoNode(*id))?;
                match &form {
                    Form::Alpha => match &mut n.kind {
                        NodeKind::Image(im) => im.key = key.clone(),
                        _ => return Err(EngineError::NoNode(*id)),
                    },
                    Form::Mask { .. } => {
                        let mut extras = n.extras_mut();
                        let m = extras.mask.as_deref_mut().ok_or(EngineError::NoNode(*id))?;
                        match &mut std::sync::Arc::make_mut(&mut m.art).kind {
                            NodeKind::Image(mi) => mi.key = key.clone(),
                            _ => return Err(EngineError::NoNode(*id)),
                        }
                    }
                }
            }
            Ok(())
        })?;
    }
    Ok(json!({
        "id": id.0,
        "ids": ids.iter().map(|i| i.0).collect::<Vec<_>>(),
        "form": if form == Form::Alpha { "alpha" } else { "mask" },
        "width": size.0,
        "height": size.1,
        "level": level,
        "softPixels": counts.soft,
        "clearPixels": counts.clear,
        "opaquePixels": counts.opaque,
    }))
}
