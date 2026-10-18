//! Bounded, source-space image samples. Decode and masks are applied before any
//! ink prediction. These samples do not by themselves resolve clipping, blending,
//! optional content, interpolation or a manufacturing hard-edge requirement.
use craft_pdf::syntax::{
    Filter,
    object::{Array, Dict, Name, Object, Stream},
    page::Resources,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Document-wide work budget: decoded source components plus normalized tone/alpha
/// evaluations. Packed rows and masks are retained, never per-pixel f64 arrays.
/// The charges bound retained sample bytes to this ceiling even for 16-bit rows.
/// A limit is Incomplete, never empty paint.
pub(crate) const SAMPLE_BUDGET: usize = 256 * 1024 * 1024;
#[derive(Clone, Debug, Serialize)]
pub struct PrintImageSamples {
    pub width: u32,
    pub height: u32,
    /// Exact colourants. Gray maps to Black; named spaces keep zero named values.
    pub colourants: Vec<String>,
    pub direct_cmyk: bool,
    pub sample_sha256: String,
    pub minimum_alpha: f64,
    pub maximum_alpha: f64,
    #[serde(skip)]
    raw: Raw,
    #[serde(skip)]
    space: Space,
    #[serde(skip)]
    mask: Mask,
}
impl PrintImageSamples {
    /// Byte storage only; sample evaluation stays in source precision.
    pub fn retained_sample_bytes(&self) -> usize {
        self.raw.bytes.len()
            + self.space.palette.as_ref().map_or(0, |p| p.len() * 8)
            + match &self.mask {
                Mask::Soft(m) | Mask::Stencil(m) => m.bytes.len(),
                Mask::Key(r) => r.len() * 8,
                Mask::Opaque => 0,
            }
    }
    pub fn tone(&self, pixel: usize, channel: usize) -> Result<f64, String> {
        if pixel >= self.raw.pixels() || channel >= self.space.names.len() {
            return Err("Image sample index outside dimensions".into());
        }
        if let Some(palette) = &self.space.palette {
            let components = self.space.names.len();
            let max = palette.len() / components - 1;
            let index = decoded(&self.raw, pixel)?.round().clamp(0.0, max as f64) as usize;
            palette.get(index * components + channel).copied().ok_or("Indexed lookup exceeds palette".into())
        } else {
            let v = decoded(&self.raw, pixel * self.raw.components + channel)?.clamp(0.0, 1.0);
            Ok(if self.space.gray { 1.0 - v } else { v })
        }
    }
    pub fn alpha(&self, pixel: usize) -> Result<f64, String> {
        if pixel >= self.raw.pixels() {
            return Err("Image alpha index outside dimensions".into());
        }
        match &self.mask {
            Mask::Opaque => Ok(1.0),
            Mask::Soft(mask) => Ok(decoded(mask, pixel)?.clamp(0.0, 1.0)),
            Mask::Stencil(mask) => Ok(1.0 - decoded(mask, pixel)?.clamp(0.0, 1.0)),
            Mask::Key(ranges) => {
                for (channel, range) in ranges.as_chunks::<2>().0.iter().enumerate() {
                    let value = f64::from(self.raw.value(pixel * self.raw.components + channel)?);
                    if value < range[0] || value > range[1] {
                        return Ok(1.0);
                    }
                }
                Ok(0.0)
            }
        }
    }
}
#[derive(Clone, Debug)]
enum Mask {
    Opaque,
    Soft(Raw),
    Stencil(Raw),
    Key(Vec<f64>),
}
fn key<'a>(dict: &Dict<'_>, long: &'a [u8], short: &'a [u8], inline: bool) -> &'a [u8] {
    if !inline || dict.contains_key(long) { long } else { short }
}
fn numbers(dict: &Dict<'_>, key: &[u8], limit: usize) -> Result<Option<Vec<f64>>, String> {
    if !dict.contains_key(key) {
        return Ok(None);
    }
    let a = dict.get::<Array<'_>>(key).ok_or("Image numeric array is unreadable")?;
    if !crate::import_color::readable(&a) {
        return Err("Image numeric array is malformed".into());
    }
    let values: Vec<_> = a.iter::<Object<'_>>().take(limit + 1).collect();
    if values.len() > limit {
        return Err("Image numeric array exceeds budget".into());
    }
    values
        .into_iter()
        .map(|n| match n {
            Object::Number(n) if n.as_f64().is_finite() => Ok(n.as_f64()),
            _ => Err("Image array contains non-numeric value".into()),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}
#[derive(Clone, Debug)]
struct Space {
    names: Vec<String>,
    gray: bool,
    direct_cmyk: bool,
    palette: Option<Box<[f64]>>,
}
fn space<'a>(value: Object<'a>, res: &Resources<'a>, depth: u8) -> Result<Space, String> {
    if depth > 4 {
        return Err("Image colour-space recursion exceeds budget".into());
    }
    match value {
        Object::Name(name) => {
            let (names, gray, direct, default) = match name.as_ref() {
                b"DeviceCMYK" | b"CMYK" => (vec!["Cyan", "Magenta", "Yellow", "Black"], false, true, b"DefaultCMYK".as_slice()),
                b"DeviceGray" | b"G" => (vec!["Black"], true, false, b"DefaultGray".as_slice()),
                b"DeviceRGB" | b"RGB" => return Err("RGB image requires qualified profile conversion".into()),
                _ => return space(res.get_color_space(&name).ok_or("Image colour-space resource missing")?, res, depth + 1),
            };
            if Name::new(default).and_then(|n| res.get_color_space(&n)).is_some() {
                return Err("Image default colour-space substitution is unresolved".into());
            }
            Ok(Space { names: names.into_iter().map(String::from).collect(), gray, direct_cmyk: direct, palette: None })
        }
        Object::Array(a) => {
            let mut items = a.flex_iter();
            let kind = items.next::<Name<'_>>().ok_or("Image colour-space kind missing")?;
            if matches!(kind.as_ref(), b"Indexed" | b"I") {
                let mut base = space(items.next::<Object<'_>>().ok_or("Indexed base missing")?, res, depth + 1)?;
                if base.palette.is_some() {
                    return Err("Nested Indexed image space is invalid".into());
                }
                let max = items.next::<u16>().filter(|n| *n <= 255).ok_or("Invalid Indexed high value")?;
                let size = (usize::from(max) + 1) * base.names.len();
                let lookup = items.next::<Object<'_>>().ok_or("Indexed lookup missing")?;
                let bytes = match lookup {
                    Object::String(s) => {
                        if s.as_bytes().len() != size {
                            return Err("Indexed lookup length differs from declared palette".into());
                        }
                        s.as_bytes().to_vec()
                    }
                    Object::Stream(s) => s.decoded_within(size + 1).map_err(|_| "Indexed table exceeds decode budget")?.into_owned(),
                    _ => return Err("Indexed lookup is not a string or stream".into()),
                };
                if bytes.len() != size {
                    return Err("Indexed lookup length differs from declared palette".into());
                }
                base.palette = Some(bytes.iter().map(|b| if base.gray { 1.0 - f64::from(*b) / 255.0 } else { f64::from(*b) / 255.0 }).collect());
                // OPM 1 suppression applies only to direct DeviceCMYK, not Indexed.
                base.direct_cmyk = false;
                Ok(base)
            } else if matches!(kind.as_ref(), b"Separation" | b"DeviceN") {
                let names = if kind.as_ref() == b"Separation" {
                    vec![items.next::<Name<'_>>().ok_or("Separation colourant missing")?]
                } else {
                    let a = items.next::<Array<'_>>().ok_or("DeviceN colourants missing")?;
                    if !crate::import_color::readable(&a) {
                        return Err("DeviceN colourant array unreadable".into());
                    }
                    let names: Vec<_> = a.iter::<Name<'_>>().take(33).collect();
                    if names.len() != crate::import_color::items(&a).count() {
                        return Err("DeviceN colourant is not a name".into());
                    }
                    names
                };
                if names.is_empty() || names.len() > 32 || names.iter().map(|n| n.as_ref().len()).sum::<usize>() > 1024 {
                    return Err("Image colourant budget exceeded".into());
                }
                let names = names
                    .into_iter()
                    .map(|n| {
                        std::str::from_utf8(n.as_ref())
                            .map(str::to_owned)
                            .map_err(|_| "Non-UTF8 image colourant requires byte-name reconciliation".to_owned())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if names.iter().any(|n| n.is_empty() || n == "All" || n == "None")
                    || names.iter().collect::<std::collections::BTreeSet<_>>().len() != names.len()
                {
                    return Err("Special or duplicate image colourants require reconciliation".into());
                }
                Ok(Space { names, gray: false, direct_cmyk: false, palette: None })
            } else {
                Err("Image colour space needs qualified source-space decoding".into())
            }
        }
        _ => Err("Invalid image colour space".into()),
    }
}
#[derive(Clone, Debug)]
struct Raw {
    width: u32,
    height: u32,
    bits: u8,
    components: usize,
    bytes: Box<[u8]>,
    row_bytes: usize,
    decode: Vec<f64>,
}
impl Raw {
    fn pixels(&self) -> usize {
        self.width as usize * self.height as usize
    }
    fn value(&self, at: usize) -> Result<u16, String> {
        if at >= self.pixels() * self.components {
            return Err("Image component missing".into());
        }
        let row_samples = self.width as usize * self.components;
        let row = at / row_samples;
        let bit = (at % row_samples) * usize::from(self.bits);
        let offset = row * self.row_bytes + bit / 8;
        let a = *self.bytes.get(offset).ok_or("Image sample missing")?;
        if self.bits == 16 {
            Ok(u16::from_be_bytes([a, *self.bytes.get(offset + 1).ok_or("Image sample missing")?]))
        } else {
            Ok(u16::from(a >> (8 - usize::from(self.bits) - bit % 8)) & ((1u16 << self.bits) - 1))
        }
    }
}
fn read_raw(stream: &Stream<'_>, components: usize, inline: bool, budget: &mut usize, indexed: bool) -> Result<Raw, String> {
    let d = stream.dict();
    if [b"Intent".as_slice(), b"Alternates", b"OPI"].iter().any(|k| d.contains_key(k)) {
        return Err("Image rendering intent or alternate print source requires qualification".into());
    }
    let width = d.get::<u32>(key(d, b"Width", b"W", inline)).filter(|n| *n > 0).ok_or("Invalid image width")?;
    let height = d.get::<u32>(key(d, b"Height", b"H", inline)).filter(|n| *n > 0).ok_or("Invalid image height")?;
    let bits = d
        .get::<u8>(key(d, b"BitsPerComponent", b"BPC", inline))
        .or_else(|| d.get::<bool>(key(d, b"ImageMask", b"IM", inline)).filter(|v| *v).map(|_| 1))
        .filter(|b| [1, 2, 4, 8, 16].contains(b))
        .ok_or("Invalid image bit depth")?;
    if indexed && bits == 16 {
        return Err("16-bit Indexed samples require independent precision qualification".into());
    }
    // Lossy filters have separate decode/colour-transform semantics; do not pretend
    // their post-filter bytes are source samples. Flate predictors are decoded by the reader.
    if stream.filters().iter().any(|f| matches!(f, Filter::DctDecode | Filter::JpxDecode | Filter::Jbig2Decode | Filter::CcittFaxDecode)) {
        return Err("Image compression requires qualified sample decoding".into());
    }
    let count =
        u64::from(width).checked_mul(u64::from(height)).and_then(|n| n.checked_mul(components as u64)).ok_or("Image sample count overflow")?;
    if count > *budget as u64 {
        return Err("Image decoded sample budget exceeded".into());
    }
    *budget -= count as usize;
    let row_bits = u64::from(width) * components as u64 * u64::from(bits);
    let row_bytes = row_bits.div_ceil(8) as usize;
    let length = row_bytes.checked_mul(height as usize).ok_or("Image byte length overflow")?;
    let mut bytes = stream.decoded_within(length + 1).map_err(|_| "Image sample stream exceeds budget or is malformed")?;
    if inline && stream.filters().is_empty() {
        // The shared reader includes the white-space separator before EI in an
        // unfiltered inline stream. Dimension-derived length identifies sample
        // bytes; never trim sample-valued spaces or silently discard extra data.
        let suffix = bytes.get(length..).ok_or("Inline image samples are shorter than declared dimensions")?;
        let whitespace = |b: u8| matches!(b, 0 | 9 | 10 | 12 | 13 | 32);
        if !(suffix.len() == 1 && suffix.first().is_some_and(|b| whitespace(*b)) || suffix == b"\r\n") {
            return Err("Inline image sample length or EI separator is unresolved".into());
        }
        bytes = std::borrow::Cow::Owned(bytes.get(..length).ok_or("Inline image samples missing")?.to_vec());
    }
    if bytes.len() != length {
        return Err("Image sample length differs from declared dimensions".into());
    }
    let end = if indexed { ((1u32 << bits) - 1) as f64 } else { 1.0 };
    let decode = numbers(d, key(d, b"Decode", b"D", inline), 64)?.unwrap_or_else(|| (0..components).flat_map(|_| [0.0, end]).collect());
    if decode.len() != components * 2 {
        return Err("Image Decode array length differs from source components".into());
    }
    if indexed && decode != [0.0, end] && decode != [end, 0.0] {
        return Err("Fractional Indexed Decode requires renderer-specific sample qualification".into());
    }
    Ok(Raw { width, height, bits, components, bytes: bytes.into_owned().into_boxed_slice(), row_bytes, decode })
}
fn decoded(raw: &Raw, at: usize) -> Result<f64, String> {
    let value = f64::from(raw.value(at)?) / f64::from((1u32 << raw.bits) - 1);
    let channel = at % raw.components;
    let low = *raw.decode.get(channel * 2).ok_or("Image Decode low missing")?;
    let high = *raw.decode.get(channel * 2 + 1).ok_or("Image Decode high missing")?;
    let result = low + value * (high - low);
    if !result.is_finite() {
        return Err("Image Decode arithmetic is not finite".into());
    }
    Ok(result)
}
pub(crate) fn decode(
    stream: &Stream<'_>,
    res: &Resources<'_>,
    inline: bool,
    description: &crate::PrintImageDescription,
    budget: &mut usize,
) -> Result<PrintImageSamples, String> {
    if !description.issues.is_empty() || description.external_data || description.embedded_alpha || description.stencil {
        return Err("Image context requires explicit decoding beyond direct samples".into());
    }
    let d = stream.dict();
    let space = space(d.get::<Object<'_>>(key(d, b"ColorSpace", b"CS", inline)).ok_or("Image colour space missing")?, res, 0)?;
    let components = space.names.len();
    let raw = read_raw(stream, if space.palette.is_some() { 1 } else { components }, inline, budget, space.palette.is_some())?;
    let pixels = raw.width as usize * raw.height as usize;
    let count = pixels
        .checked_mul(components + 1)
        .and_then(|n| n.checked_add(space.palette.as_ref().map_or(0, |p| p.len() * 8)))
        .ok_or("Image sample work overflow")?;
    if count > *budget {
        return Err("Image normalized sample work budget exceeded".into());
    }
    *budget -= count;
    let retained_mask;
    let soft = d.get::<Stream<'_>>(b"SMask");
    if d.contains_key(b"SMask") && soft.is_none() {
        return Err("Image soft mask is unreadable".into());
    }
    if let Some(mask) = soft {
        let md = mask.dict();
        if md.contains_key(b"Matte")
            || md.contains_key(b"Mask")
            || md.contains_key(b"SMask")
            || md.contains_key(b"SMaskInData")
            || md.contains_key(b"OC")
            || md.contains_key(b"F")
        {
            return Err("Nested, external or matte image soft mask requires compositing".into());
        }
        if Name::new(b"DefaultGray").and_then(|n| res.get_color_space(&n)).is_some() {
            return Err("Soft-mask DefaultGray substitution requires qualification".into());
        }
        if md.get::<Name<'_>>(b"ColorSpace").as_deref() != Some(b"DeviceGray")
            || (md.contains_key(b"Interpolate") && md.get::<bool>(b"Interpolate") != Some(false))
        {
            return Err("Soft mask requires direct gray nearest sample semantics".into());
        }
        let mask = read_raw(&mask, 1, false, budget, false)?;
        if mask.width != raw.width || mask.height != raw.height {
            return Err("Soft-mask resampling requires separate coverage evidence".into());
        }
        retained_mask = Mask::Soft(mask);
    } else if let Some(mask) = d.get::<Stream<'_>>(b"Mask") {
        let md = mask.dict();
        if md.get::<bool>(b"ImageMask") != Some(true)
            || (md.contains_key(b"Interpolate") && md.get::<bool>(b"Interpolate") != Some(false))
            || [b"Mask".as_slice(), b"SMask", b"SMaskInData", b"OC", b"F"].iter().any(|k| md.contains_key(k))
        {
            return Err("Explicit image mask requires a direct noninterpolated stencil".into());
        }
        let mask = read_raw(&mask, 1, false, budget, false)?;
        if mask.width != raw.width || mask.height != raw.height {
            return Err("Explicit-mask resampling requires separate coverage evidence".into());
        }
        if mask.decode != [0.0, 1.0] && mask.decode != [1.0, 0.0] {
            return Err("Explicit stencil mask requires binary Decode endpoints".into());
        }
        if mask.bits != 1 {
            return Err("Image mask must have one-bit samples".into());
        }
        retained_mask = Mask::Stencil(mask);
    } else if d.contains_key(b"Mask")
        && let Some(ranges) = numbers(d, b"Mask", 64)?
    {
        if raw.bits == 16 {
            return Err("16-bit colour-key mask requires independent precision qualification".into());
        }
        if ranges.len() != raw.components * 2
            || ranges
                .as_chunks::<2>()
                .0
                .iter()
                .any(|r| r[0] < 0.0 || r[0] > r[1] || r[1] > f64::from((1u32 << raw.bits) - 1) || r.iter().any(|n| n.fract() != 0.0))
        {
            return Err("Invalid image color-key mask".into());
        }
        retained_mask = Mask::Key(ranges);
    } else {
        retained_mask = Mask::Opaque;
    }
    let mut samples = PrintImageSamples {
        width: raw.width,
        height: raw.height,
        colourants: space.names.clone(),
        direct_cmyk: space.direct_cmyk,
        sample_sha256: String::new(),
        minimum_alpha: 1.0,
        maximum_alpha: 0.0,
        raw,
        space,
        mask: retained_mask,
    };
    let mut hash = Sha256::new();
    hash.update(b"vectorcraft-image-samples-v1\0");
    hash.update(samples.width.to_be_bytes());
    hash.update(samples.height.to_be_bytes());
    for name in &samples.colourants {
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
    }
    // Preserve v1 normalized evidence identity while evaluating one component at a
    // time. Changing storage must not change what a sample hash represents.
    for pixel in 0..pixels {
        for channel in 0..components {
            hash.update(samples.tone(pixel, channel)?.to_be_bytes());
        }
    }
    for pixel in 0..pixels {
        let alpha = samples.alpha(pixel)?;
        samples.minimum_alpha = samples.minimum_alpha.min(alpha);
        samples.maximum_alpha = samples.maximum_alpha.max(alpha);
        hash.update(alpha.to_be_bytes());
    }
    samples.sample_sha256 = format!("{:x}", hash.finalize());
    Ok(samples)
}
