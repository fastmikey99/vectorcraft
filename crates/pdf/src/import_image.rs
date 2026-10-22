//! CMYK images keep their samples, as CMYK colours do: DeviceCMYK images and ICC-based ones with
//! four components, plus Separation/DeviceN images, whose process inks are kept as CMYK and whose
//! spot inks become spot channels ([`vectorcraft_doc::cmyk::spots`]) printing on the plates of the
//! spot swatches paints in the same inks have. An image in inks that is read as RGB after all says
//! why in a warning. A JPEG whose Decode array inverts its samples (Adobe's CMYK JPEGs, the
//! convention JPEG readers and the PDF writer take a CMYK JPEG to follow) is kept as it is; any
//! other is decoded, its Decode array applied, and stored as a CMYK TIFF
//! ([`ImageBlob::cmyk_tiff`]). A mask (`/SMask`, `/Mask`) doesn't change that: the importer
//! carries it as the image's opacity mask ([`has_mask`]). Every other image is read as RGB by the
//! interpreter.

use craft_pdf::syntax::Filter;
use craft_pdf::syntax::object::stream::ImageDecodeParams;
use craft_pdf::syntax::object::{Array, Dict, Name, Object, Stream};
use vectorcraft_doc::ImageBlob;
use vectorcraft_doc::cmyk::spots::MAX_SPOTS;
use vectorcraft_doc::cmyk::{Inks, MAX_CMYK_PIXELS, SpotInks, jpeg_components};

use crate::import_color::{Colors, ImageChannel};

/// A CMYK image read as RGB: why.
pub(crate) const UNREAD: &str = "CMYK images whose samples couldn't be read in CMYK were imported in RGB";
/// An image in spot inks read as RGB: why.
pub(crate) const INK_UNREAD: &str = "images in spot inks whose samples couldn't be read were imported in RGB, separated into process inks";
const INDEXED: &str = "indexed images in CMYK or spot inks were imported in RGB, separated into process inks";
const TOO_MANY: &str = "images in more than 28 spot inks were imported in RGB, separated into process inks";
const NO_PLATES: &str = "images in inks that print on no plate were imported in RGB";

/// The Decode array of a JPEG kept as it is.
const INVERTED: [(f32, f32); 4] = [(1.0, 0.0); 4];

/// `key`, or its inline-image abbreviation `short` when `d` doesn't have it.
fn key<'k>(d: &Dict<'_>, key: &'k [u8], short: &'k [u8]) -> &'k [u8] {
    if d.contains_key(key) { key } else { short }
}

/// Where a stored sample goes: a process ink (CMYK channel), a spot channel (index into the
/// image's spot names), every plate, or nowhere.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Plate {
    Process(usize),
    Spot(usize),
    All,
    None,
}

/// How an image's samples print: each one's plate, and the spot channels' swatch names.
struct Channels {
    plates: Vec<Plate>,
    spots: Vec<String>,
}

/// The plates of image `d`'s samples: `Ok(None)` when it isn't a CMYK image or one in inks
/// (Separation, DeviceN), `Err` (why, a warning) when it is one that is read as RGB. Named
/// process inks are real plates, and spot inks print on their own plates; the alternate space's
/// tint transform is only a fallback preview.
fn channels(d: &Dict<'_>, colors: &mut Colors<'_>) -> Result<Option<Channels>, &'static str> {
    let process = |p: Vec<usize>| Ok(Some(Channels { plates: p.into_iter().map(Plate::Process).collect(), spots: vec![] }));
    let cs = key(d, b"ColorSpace", b"CS");
    if let Some(n) = d.get::<Name<'_>>(cs) {
        return if matches!(n.as_ref(), b"DeviceCMYK" | b"CMYK") { process(vec![0, 1, 2, 3]) } else { Ok(None) };
    }
    let Some(a) = d.get::<Array<'_>>(cs) else { return Ok(None) };
    let mut it = a.flex_iter();
    let Some(kind) = it.next::<Name<'_>>() else { return Ok(None) };
    match kind.as_ref() {
        b"CalCMYK" => process(vec![0, 1, 2, 3]),
        b"ICCBased" => match it.next::<Stream<'_>>().and_then(|s| s.dict().get::<u8>(b"N")) {
            Some(4) => process(vec![0, 1, 2, 3]),
            _ => Ok(None),
        },
        b"Separation" | b"DeviceN" => ink_channels(&a, colors).map(Some),
        b"Indexed" | b"I" => {
            let base = it.next::<Object<'_>>();
            let inks = match &base {
                Some(Object::Name(n)) => matches!(n.as_ref(), b"DeviceCMYK" | b"CMYK"),
                Some(Object::Array(b)) => {
                    let mut bi = b.flex_iter();
                    match bi.next::<Name<'_>>() {
                        Some(n) if matches!(n.as_ref(), b"Separation" | b"DeviceN" | b"CalCMYK") => true,
                        Some(n) if n.as_ref() == b"ICCBased" => bi.next::<Stream<'_>>().and_then(|s| s.dict().get::<u8>(b"N")) == Some(4),
                        _ => false,
                    }
                }
                _ => false,
            };
            if inks { Err(INDEXED) } else { Ok(None) }
        }
        _ => Ok(None),
    }
}

/// [`channels`] of a Separation or DeviceN space `a`.
fn ink_channels(a: &Array<'_>, colors: &mut Colors<'_>) -> Result<Channels, &'static str> {
    let mut plates = Vec::new();
    let mut spots: Vec<String> = Vec::new();
    for c in colors.image_channels(a)? {
        plates.push(match c {
            ImageChannel::Process(ch) => Plate::Process(ch),
            ImageChannel::All => Plate::All,
            ImageChannel::None => Plate::None,
            ImageChannel::Spot(name) => match spots.iter().position(|s| *s == name) {
                Some(i) => Plate::Spot(i),
                None => {
                    spots.push(name);
                    Plate::Spot(spots.len() - 1)
                }
            },
        });
    }
    if spots.len() > MAX_SPOTS {
        return Err(TOO_MANY);
    }
    if plates.is_empty() || plates.iter().all(|p| *p == Plate::None) {
        return Err(NO_PLATES);
    }
    Ok(Channels { plates, spots })
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

/// The image `st` (`width` × `height` pixels) in CMYK, with its spot channels: `Ok(None)` when it
/// isn't a CMYK image or one in inks, `Err` (a warning) when it is one that is read as RGB.
pub(crate) fn cmyk(st: &Stream<'_>, width: u32, height: u32, colors: &mut Colors<'_>) -> Result<Option<ImageBlob>, &'static str> {
    let d = st.dict();
    let Some(Channels { plates, spots }) = channels(d, colors)? else { return Ok(None) };
    if plates.iter().any(|p| matches!(p, Plate::All)) {
        return Err("registration /All image cannot preserve coverage on every output colorant, including document spots");
    }
    let unread = if spots.is_empty() { UNREAD } else { INK_UNREAD };
    let count = plates.len();
    let ranges = decode_ranges(d, count);
    let filters = st.filters();
    let process: Vec<usize> = plates.iter().filter_map(|p| if let Plate::Process(c) = p { Some(*c) } else { None }).collect();
    let plain = process == [0, 1, 2, 3] && count == 4;
    if plain && ranges == INVERTED && matches!(filters.as_slice(), [Filter::DctDecode]) {
        let raw = st.raw_data();
        if jpeg_components(&raw) == Some(4) {
            return Ok(Some(ImageBlob::new("image/jpeg", raw.into_owned())));
        }
    }
    let bpc = d.get::<u8>(key(d, b"BitsPerComponent", b"BPC")).unwrap_or(8);
    if !matches!(bpc, 8 | 16) || count > usize::from(u8::MAX) {
        return Err(unread);
    }
    let n = width as u64 * height as u64;
    if n == 0 || n > MAX_CMYK_PIXELS {
        return Err(unread);
    }
    let size = usize::from(bpc / 8);
    let params = ImageDecodeParams { is_indexed: false, bpc: Some(bpc), num_components: Some(count as u8), target_dimension: None, width, height };
    let Ok(decoded) = st.decoded_image(&params) else { return Err(unread) };
    let Some(raw) = decoded.data.get(..n as usize * count * size) else { return Err(unread) };
    let value = |s: &[u8]| match *s {
        [hi, lo] => u16::from_be_bytes([hi, lo]) as f32 / 65535.0,
        [v] => v as f32 / 255.0,
        _ => 0.0,
    };
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut inks = Vec::with_capacity(n as usize * 4);
    let mut spot = Vec::with_capacity(n as usize * spots.len());
    for px in raw.chunks_exact(size * count) {
        let mut ink = [0u8; 4];
        let mut tints = [0u8; MAX_SPOTS];
        let mut all = 0u8;
        for ((sample, &(lo, hi)), plate) in px.chunks_exact(size).zip(&ranges).zip(&plates) {
            let v = byte(lo + value(sample) * (hi - lo));
            match *plate {
                Plate::Process(c) => {
                    if let Some(out) = ink.get_mut(c) {
                        *out = (*out).max(v);
                    }
                }
                Plate::Spot(i) => {
                    if let Some(out) = tints.get_mut(i) {
                        *out = (*out).max(v);
                    }
                }
                Plate::All => all = all.max(v),
                Plate::None => {}
            }
        }
        // Registration prints on every plate: the process inks and the image's spot inks.
        inks.extend(ink.map(|v| v.max(all)));
        spot.extend(tints.iter().take(spots.len()).map(|v| (*v).max(all)));
    }
    let inks = Inks::new(width, height, inks).ok_or(unread)?;
    if !spots.is_empty() {
        let spots = SpotInks::new(width, height, spots, spot).ok_or(unread)?;
        return ImageBlob::cmyk_spot_tiff(&inks, &spots).map(Some).ok_or(unread);
    }
    let mut blob = ImageBlob::cmyk_tiff(&inks).ok_or(unread)?;
    // Preserve only unambiguous DeviceCMYK JPEGs. Other colour spaces and decode transforms
    // keep their normalized, lossless working samples instead.
    if bpc == 8
        && plain
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

/// Does the image carry its own transparency (a soft mask, a stencil mask or a colour key)?
pub(crate) fn has_mask(st: &Stream<'_>) -> bool {
    let d = st.dict();
    d.contains_key(b"SMask") || d.contains_key(b"Mask") || d.get::<u8>(b"SMaskInData").is_some_and(|v| v != 0)
}
