//! CMYK images keep their samples, as CMYK colours do: DeviceCMYK images and ICC-based ones with
//! four components, plus Separation/DeviceN images containing only named process inks. A JPEG whose Decode array inverts its samples (Adobe's CMYK JPEGs, the
//! convention JPEG readers and the PDF writer take a CMYK JPEG to follow) is kept as it is; any
//! other is decoded, its Decode array applied, and stored as a CMYK TIFF
//! ([`ImageBlob::cmyk_tiff`]). A mask (`/SMask`, `/Mask`) doesn't change that: the importer
//! carries it as the image's opacity mask ([`has_mask`]). Every other image is read as RGB by the
//! interpreter.

use craft_pdf::syntax::Filter;
use craft_pdf::syntax::object::stream::ImageDecodeParams;
use craft_pdf::syntax::object::{Array, Dict, Name, Object, Stream};
use vectorcraft_doc::ImageBlob;
use vectorcraft_doc::cmyk::{Inks, MAX_CMYK_PIXELS, jpeg_components};

/// A CMYK image read as RGB: why.
pub(crate) const UNREAD: &str = "CMYK images whose samples couldn't be read in CMYK were imported in RGB";

/// Only explicit device-Gray bases qualify for a Gray working image. Neutral
/// RGB, calibrated/ICC Gray and a Separation named Black are different spaces.
/// Resource substitutions and nested mask/pattern interpretation are excluded
/// by the caller; this predicate never resolves an arbitrary colour-space name.
pub(crate) fn device_gray(st: &Stream<'_>) -> bool {
    let d = st.dict();
    if d.contains_key(b"ColorSpace") && d.contains_key(b"CS") {
        return false;
    }
    let cs = key(d, b"ColorSpace", b"CS");
    if let Some(n) = d.get::<Name<'_>>(cs) {
        return matches!(n.as_ref(), b"DeviceGray" | b"G");
    }
    let Some(a) = d.get::<Array<'_>>(cs) else { return false };
    let mut it = a.flex_iter();
    a.iter::<Object<'_>>().take(5).count() == 4
        && it.next::<Name<'_>>().is_some_and(|n| matches!(n.as_ref(), b"Indexed" | b"I"))
        && it.next::<Name<'_>>().is_some_and(|n| matches!(n.as_ref(), b"DeviceGray" | b"G"))
}

/// The Decode array of a JPEG kept as it is.
const INVERTED: [(f32, f32); 4] = [(1.0, 0.0); 4];

/// `key`, or its inline-image abbreviation `short` when `d` doesn't have it.
fn key<'k>(d: &Dict<'_>, key: &'k [u8], short: &'k [u8]) -> &'k [u8] {
    if d.contains_key(key) { key } else { short }
}

/// Map each stored sample to its process-ink channel. Named process inks are real
/// plates; their alternate-space tint transform is only a fallback preview.
fn channels(d: &Dict<'_>) -> Option<Vec<usize>> {
    let cs = key(d, b"ColorSpace", b"CS");
    if let Some(n) = d.get::<Name<'_>>(cs) {
        return matches!(n.as_ref(), b"DeviceCMYK" | b"CMYK").then(|| vec![0, 1, 2, 3]);
    }
    let a = d.get::<Array<'_>>(cs)?;
    let mut it = a.flex_iter();
    let kind = it.next::<Name<'_>>()?;
    match kind.as_ref() {
        b"CalCMYK" => Some(vec![0, 1, 2, 3]),
        b"ICCBased" => (it.next::<Stream<'_>>()?.dict().get::<u8>(b"N") == Some(4)).then(|| vec![0, 1, 2, 3]),
        b"Separation" => process_channel(it.next::<Name<'_>>()?.as_ref()).map(|c| vec![c]),
        b"DeviceN" => {
            let names = it.next::<Array<'_>>()?;
            let mut mapped = Vec::new();
            for name in names.iter::<Name<'_>>() {
                let channel = process_channel(name.as_ref())?;
                if mapped.len() == 4 || mapped.contains(&channel) {
                    return None;
                }
                mapped.push(channel);
            }
            (!mapped.is_empty() && mapped.len() == names.iter::<Object<'_>>().count()).then_some(mapped)
        }
        _ => None,
    }
}

fn process_channel(name: &[u8]) -> Option<usize> {
    match name {
        b"Cyan" => Some(0),
        b"Magenta" => Some(1),
        b"Yellow" => Some(2),
        b"Black" => Some(3),
        _ => None,
    }
}

/// Each stored sample's Decode range. Omitted channels always remain zero ink.
fn decode_ranges(d: &Dict<'_>, count: usize) -> Vec<(f32, f32)> {
    let fallback = vec![(0.0, 1.0); count];
    let Some(a) = d.get::<Array<'_>>(key(d, b"Decode", b"D")) else { return fallback };
    let v: Vec<f32> = a.iter::<f32>().collect();
    if v.len() != count * 2 || v.iter().any(|x| !x.is_finite()) {
        return fallback;
    }
    v.as_chunks::<2>().0.iter().map(|&[start, end]| (start, end)).collect()
}

/// The image `st` (`width` × `height` pixels) in CMYK: `Ok(None)` when it isn't a CMYK image,
/// `Err` (a warning) when it is one that is read as RGB.
pub(crate) fn cmyk(st: &Stream<'_>, width: u32, height: u32) -> Result<Option<ImageBlob>, &'static str> {
    let d = st.dict();
    if let Some(blob) = indexed_cmyk(st, width, height)? {
        return Ok(Some(blob));
    }
    let Some(channels) = channels(d) else { return Ok(None) };
    let count = channels.len();
    let ranges = decode_ranges(d, count);
    let filters = st.filters();
    if channels == [0, 1, 2, 3] && ranges == INVERTED && matches!(filters.as_slice(), [Filter::DctDecode]) {
        let raw = st.raw_data();
        if jpeg_components(&raw) == Some(4) {
            return Ok(Some(ImageBlob::new("image/jpeg", raw.into_owned())));
        }
    }
    let bpc = d.get::<u8>(key(d, b"BitsPerComponent", b"BPC")).unwrap_or(8);
    if !matches!(bpc, 8 | 16) {
        return Err(UNREAD);
    }
    let n = width as u64 * height as u64;
    if n == 0 || n > MAX_CMYK_PIXELS {
        return Err(UNREAD);
    }
    let size = usize::from(bpc / 8);
    let params = ImageDecodeParams { is_indexed: false, bpc: Some(bpc), num_components: Some(count as u8), target_dimension: None, width, height };
    let Ok(decoded) = st.decoded_image(&params) else { return Err(UNREAD) };
    let Some(raw) = decoded.data.get(..n as usize * count * size) else { return Err(UNREAD) };
    let value = |s: &[u8]| match *s {
        [hi, lo] => u16::from_be_bytes([hi, lo]) as f32 / 65535.0,
        [v] => v as f32 / 255.0,
        _ => 0.0,
    };
    let inks = raw
        .chunks_exact(size * count)
        .flat_map(|px| {
            let mut ink = [0u8; 4];
            for ((sample, &(lo, hi)), &channel) in px.chunks_exact(size).zip(&ranges).zip(&channels) {
                if let Some(out) = ink.get_mut(channel) {
                    *out = ((lo + value(sample) * (hi - lo)).clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
            ink
        })
        .collect();
    let mut blob = Inks::new(width, height, inks).and_then(|i| ImageBlob::cmyk_tiff(&i)).ok_or(UNREAD)?;
    // Preserve only unambiguous DeviceCMYK JPEGs. Other colour spaces and decode transforms
    // keep their normalized, lossless working samples instead.
    if bpc == 8
        && channels == [0, 1, 2, 3]
        && ranges == [(0.0, 1.0); 4]
        && matches!(filters.as_slice(), [Filter::DctDecode])
        && d.get::<Name<'_>>(key(d, b"ColorSpace", b"CS")).is_some_and(|n| matches!(n.as_ref(), b"DeviceCMYK" | b"CMYK"))
        && !d.contains_key(b"DecodeParms")
        && !d.contains_key(b"DP")
    {
        let raw = st.raw_data();
        if jpeg_components(&raw) == Some(4) {
            blob.source_jpeg =
                Some(vectorcraft_doc::PreservedJpeg { bytes: std::sync::Arc::new(raw.into_owned()), decoded_key: blob.content_key(), invert: false });
        }
    }
    Ok(Some(blob))
}

/// Expand an Indexed DeviceCMYK palette without a preview RGB conversion. PDF samples
/// are MSB-first, rows are byte-padded, and Decode maps indices before hival clamping.
fn indexed_cmyk(st: &Stream<'_>, width: u32, height: u32) -> Result<Option<ImageBlob>, &'static str> {
    let d = st.dict();
    let Some(space) = d.get::<Array<'_>>(key(d, b"ColorSpace", b"CS")) else { return Ok(None) };
    let mut items = space.flex_iter();
    if !items.next::<Name<'_>>().is_some_and(|n| matches!(n.as_ref(), b"Indexed" | b"I"))
        || !items.next::<Name<'_>>().is_some_and(|n| matches!(n.as_ref(), b"DeviceCMYK" | b"CMYK"))
    {
        return Ok(None);
    }
    let high = items.next::<u32>().filter(|n| *n <= 255).ok_or(UNREAD)? as usize;
    let palette = items
        .next::<Stream<'_>>()
        .and_then(|s| s.decoded().ok().map(|b| b.into_owned()))
        .or_else(|| items.next::<craft_pdf::syntax::object::String<'_>>().map(|s| s.to_vec()))
        .ok_or(UNREAD)?;
    if palette.len() < (high + 1) * 4 {
        return Err(UNREAD);
    }
    let bpc = d.get::<u8>(key(d, b"BitsPerComponent", b"BPC")).ok_or(UNREAD)?;
    let count = u64::from(width) * u64::from(height);
    if !matches!(bpc, 1 | 2 | 4 | 8) || count == 0 || count > MAX_CMYK_PIXELS {
        return Err(UNREAD);
    }
    let max = (1_u16 << bpc) - 1;
    let (lo, hi) = if let Some(decode) = d.get::<Array<'_>>(key(d, b"Decode", b"D")) {
        let values: Vec<f64> = decode.iter::<f64>().collect();
        match values.as_slice() {
            [lo, hi] if lo.is_finite() && hi.is_finite() => (*lo, *hi),
            _ => return Err(UNREAD),
        }
    } else {
        (0.0, f64::from(max))
    };
    let params = ImageDecodeParams { is_indexed: true, bpc: Some(bpc), num_components: Some(1), target_dimension: None, width, height };
    let decoded = st.decoded_image(&params).map_err(|_| UNREAD)?;
    let row_bytes = (u64::from(width) * u64::from(bpc)).div_ceil(8) as usize;
    let raw = decoded.data.get(..row_bytes * height as usize).ok_or(UNREAD)?;
    let mut inks = Vec::with_capacity(count as usize * 4);
    for row in raw.chunks_exact(row_bytes) {
        for x in 0..width as usize {
            let bit = x * usize::from(bpc);
            let byte = row.get(bit / 8).ok_or(UNREAD)?;
            let sample = (u16::from(*byte) >> (8 - usize::from(bpc) - bit % 8)) & max;
            let index = (lo + f64::from(sample) / f64::from(max) * (hi - lo)).clamp(0.0, high as f64).round() as usize;
            inks.extend_from_slice(palette.get(index * 4..index * 4 + 4).ok_or(UNREAD)?);
        }
    }
    Inks::new(width, height, inks).and_then(|i| ImageBlob::cmyk_tiff(&i)).map(Some).ok_or(UNREAD)
}

/// Does the image carry its own transparency (a soft mask, a stencil mask or a colour key)?
pub(crate) fn has_mask(st: &Stream<'_>) -> bool {
    let d = st.dict();
    d.contains_key(b"SMask") || d.contains_key(b"Mask") || d.get::<u8>(b"SMaskInData").is_some_and(|v| v != 0)
}
