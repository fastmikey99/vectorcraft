//! Images in the PDF: the document's images as the Compression settings say (resampled above a
//! resolution, compressed with ZIP or JPEG) and converted as the Output settings say
//! ([`Pixels`]), and pixels the writer makes itself (freeform gradients).
//!
//! Every image is classified as the settings are split: colour, greyscale (a grey colour type) or
//! monochrome (greyscale that is only black and white). An image already as asked (not resampled,
//! a JPEG kept as JPEG or another kept lossless) is embedded as it is; the others are decoded,
//! resampled and encoded again. JPEG needs opaque pixels: images with transparency stay lossless.
//!
//! CMYK images ([`ImageBlob::cmyk`]) stay CMYK: a CMYK JPEG already as asked is embedded as it is
//! when its numbers are kept; the others' ink amounts are resampled, converted when the Output
//! settings convert CMYK colours ([`CmykPixels`]) and compressed again. CMYK JPEGs hold Adobe's
//! inverted samples, which the PDF writer's Decode array undoes.

use std::io::Cursor;
use std::sync::Arc;

use craft_pdf::write::backend as krilla;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageFormat, ImageReader};
use krilla::image::{BitsPerComponent, CustomImage, Image, ImageColorspace};

use vectorcraft_doc::ImageBlob;
use vectorcraft_doc::cmyk::jpeg_components;

use crate::output::{CmykPixels, Pixels};
use crate::{CompressionSettings, Downsample, ImageCodec, JpegQuality, MonoCodec};

/// Raw 8-bit pixels: colour samples (RGB, grey or CMYK) and an optional alpha channel.
#[derive(Clone, Hash)]
struct Raw {
    color: Arc<Vec<u8>>,
    alpha: Option<Arc<Vec<u8>>>,
    width: u32,
    height: u32,
    space: ImageColorspace,
}

impl CustomImage for Raw {
    fn color_channel(&self) -> &[u8] {
        &self.color
    }

    fn alpha_channel(&self) -> Option<&[u8]> {
        self.alpha.as_deref().map(Vec::as_slice)
    }

    fn bits_per_component(&self) -> BitsPerComponent {
        BitsPerComponent::Eight
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn icc_profile(&self) -> Option<&[u8]> {
        None
    }

    fn color_space(&self) -> ImageColorspace {
        self.space
    }
}

/// A `width` × `height` image of straight (not premultiplied) RGBA pixels, written as RGB (as
/// CMYK when `pixels` converts to CMYK), or as grey when every pixel is grey, with an alpha
/// channel only when some pixel isn't opaque. `interpolate` asks viewers to smooth it when
/// enlarged (PDF/A forbids it). `None` when `rgba` doesn't hold that many pixels.
fn from_rgba(rgba: &[u8], width: u32, height: u32, interpolate: bool, pixels: Option<&Pixels>) -> Option<Image> {
    let px = rgba.as_chunks::<4>().0;
    if px.len() as u64 != width as u64 * height as u64 || px.is_empty() {
        return None;
    }
    let (color, space) = if neutral(rgba) {
        (px.iter().map(|p| p[0]).collect(), ImageColorspace::Luma)
    } else if let Some(cmyk) = pixels.and_then(|p| p.cmyk(rgba)) {
        (cmyk, ImageColorspace::Cmyk)
    } else {
        (px.iter().flat_map(|p| [p[0], p[1], p[2]]).collect(), ImageColorspace::Rgb)
    };
    let alpha = px.iter().any(|p| p[3] < 255).then(|| Arc::new(px.iter().map(|p| p[3]).collect()));
    Image::from_custom(Raw { color: Arc::new(color), alpha, width, height, space }, interpolate).ok()
}

/// Are all of the RGBA pixels `rgba` grey?
fn neutral(rgba: &[u8]) -> bool {
    rgba.as_chunks::<4>().0.iter().all(|p| p[0] == p[1] && p[1] == p[2])
}

/// Opaque colour `samples` (grey, RGB, or CMYK ink amounts, as `space` says) written losslessly.
fn lossless(samples: Vec<u8>, space: ImageColorspace, width: u32, height: u32, interpolate: bool) -> Option<Image> {
    Image::from_custom(Raw { color: Arc::new(samples), alpha: None, width, height, space }, interpolate).ok()
}

/// Lower a plain, exactly aligned CMYK/Gray image pair to one image-owned alpha mask.
/// The caller excludes output conversion, resampling and enclosing compositing. Editable
/// mask art is left intact. Unsupported mask art keeps the ordinary luminosity-group path.
pub(crate) fn masked_cmyk(doc: &vectorcraft_doc::Document, node: &vectorcraft_doc::Node) -> Option<Image> {
    use vectorcraft_color::BlendMode;
    use vectorcraft_doc::{Knockout, NodeKind};
    let plain = |n: &vectorcraft_doc::Node| {
        n.visible
            && n.opacity == 1.0
            && n.blend == BlendMode::Normal
            && !n.isolate
            && n.knockout == Knockout::Neutral
            && !n.knockout_shape
            && n.appearance.items.is_empty()
            && n.appearance.effects.is_empty()
            && n.appearance.contents_index.is_none()
    };
    let NodeKind::Image(im) = &node.kind else { return None };
    let mask = node.extras().mask.as_deref()?;
    let NodeKind::Image(mi) = &mask.art.kind else { return None };
    if !plain(node)
        || !plain(&mask.art)
        || mask.disabled
        || !mask.clip
        || mask.invert
        || mask.art.extras().mask.is_some()
        || im.width == 0
        || im.height == 0
        || (im.width, im.height) != (mi.width, mi.height)
        || im.xf != mi.xf
        || im.xf.as_coeffs().iter().any(|v| !v.is_finite())
        || !im.xf.determinant().is_finite()
        || im.xf.determinant() == 0.0
        || u64::from(im.width) * u64::from(im.height) > vectorcraft_doc::cmyk::MAX_CMYK_PIXELS
    {
        return None;
    }
    let colour = doc.images.get(&im.key)?;
    let grey = doc.images.get(&mi.key)?;
    // JPEG preservation and colour conversion have separate policies; don't silently replace
    // either with a decoded lossless image. Only the importer's opaque grayscale PNG is used.
    if colour.mime != "image/tiff" || colour.source_jpeg.is_some() || grey.mime != "image/png" || grey.source_jpeg.is_some() {
        return None;
    }
    let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(grey.bytes.as_slice())).ok()?;
    let dimensions = decoder.dimensions();
    if dimensions != (im.width, im.height)
        || decoder.color_type() != image::ColorType::L8
        || decoder.icc_profile().ok()?.is_some()
        || decoder.gamma_value().ok()?.is_some()
    {
        return None;
    }
    let DynamicImage::ImageLuma8(alpha) = DynamicImage::from_decoder(decoder).ok()? else { return None };
    let inks = colour.cmyk()?;
    if (inks.width, inks.height) != dimensions || alpha.dimensions() != dimensions {
        return None;
    }
    let raw =
        Raw { color: Arc::new(inks.data), alpha: Some(Arc::new(alpha.into_raw())), width: im.width, height: im.height, space: ImageColorspace::Cmyk };
    // CustomImage's hash includes both channels; no shared image-key cache can alias masks.
    Image::from_custom(raw, false).ok()
}

/// [`from_rgba`] of pixels converted by `pixels` (when given).
pub(crate) fn from_rgba_in(mut rgba: Vec<u8>, width: u32, height: u32, interpolate: bool, pixels: Option<&Pixels>) -> Option<Image> {
    if let Some(p) = pixels {
        p.rgb_in_place(&mut rgba);
    }
    from_rgba(&rgba, width, height, interpolate, pixels)
}

/// The kinds of images the Compression section has settings for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Color,
    Gray,
    Mono,
}

/// How an image's pixels are compressed in the file.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Codec {
    /// JPEG images stay JPEG (at this quality when they are resampled); the others are lossless.
    Auto(u8),
    Zip,
    Jpeg(u8),
}

/// The settings for one kind of image.
struct Rule {
    downsample: Downsample,
    /// Target resolution, and the resolution above which images are resampled (ppi).
    ppi: f64,
    above: f64,
    codec: Codec,
    /// Why the codec differs from the one asked for (that one isn't available).
    instead: Option<&'static str>,
}

const UNCOMPRESSED: &str = "uncompressed images are not written: images are compressed with ZIP";
const CMYK_KEPT: &str = "a CMYK image couldn't be decoded: it is written as it is, not resampled or converted";
const CMYK_RGB: &str = "a CMYK image couldn't be decoded in CMYK: it is written in RGB";

/// JPEG quality (1–100) of a quality setting.
fn quality(q: JpegQuality) -> u8 {
    match q {
        JpegQuality::Minimum => 10,
        JpegQuality::Low => 30,
        JpegQuality::Medium => 55,
        JpegQuality::High => 80,
        JpegQuality::Maximum => 95,
    }
}

fn rule(c: &CompressionSettings, kind: Kind) -> Rule {
    if kind == Kind::Mono {
        let m = &c.mono;
        let instead = match m.compression {
            MonoCodec::Zip => None,
            MonoCodec::None => Some(UNCOMPRESSED),
            MonoCodec::CcittG3 | MonoCodec::CcittG4 => Some("CCITT compression is not supported: monochrome images are compressed with ZIP"),
            MonoCodec::RunLength => Some("Run Length compression is not supported: monochrome images are compressed with ZIP"),
        };
        return Rule { downsample: m.downsample, ppi: m.ppi, above: m.above_ppi, codec: Codec::Zip, instead };
    }
    let i = if kind == Kind::Color { &c.color } else { &c.gray };
    let (codec, instead) = match i.compression {
        ImageCodec::Auto => (Codec::Auto(quality(i.quality)), None),
        ImageCodec::Zip => (Codec::Zip, None),
        ImageCodec::Jpeg => (Codec::Jpeg(quality(i.quality)), None),
        ImageCodec::None => (Codec::Zip, Some(UNCOMPRESSED)),
        ImageCodec::Jpeg2000 => (Codec::Zip, Some("JPEG 2000 compression is not supported: those images are compressed with ZIP")),
    };
    Rule { downsample: i.downsample, ppi: i.ppi, above: i.above_ppi, codec, instead }
}

/// What [`recode`] makes of an image.
pub(crate) struct Recoded {
    /// The image to write; `None` keeps the source image as it is.
    pub image: Option<Image>,
    /// Set when the image isn't compressed as asked.
    pub warning: Option<&'static str>,
}

/// Is `img` (a grey image) only black and white?
fn bilevel(img: &DynamicImage) -> bool {
    img.to_luma_alpha8().pixels().all(|p| p[0] == 0 || p[0] == 255)
}

/// The size `r` gives an image of `w` × `h` pixels placed `size` points wide and high: a side
/// whose resolution is above the threshold goes down to the target resolution.
fn sized(r: &Rule, (w, h): (u32, u32), size: (f64, f64)) -> (u32, u32) {
    let ppi = |px: u32, pt: f64| px as f64 * 72.0 / pt.abs();
    let (px, py) = (ppi(w, size.0), ppi(h, size.1));
    let resample = r.downsample != Downsample::None && [px, py].iter().any(|p| p.is_finite() && *p > r.above);
    let side = |n: u32, p: f64| if resample && p.is_finite() && p > r.ppi { ((n as f64 * r.ppi / p).round() as u32).clamp(1, n) } else { n };
    (side(w, px), side(h, py))
}

/// Is an image of `dims` pixels placed `size` points already as `r` asks: not resampled, and
/// lossless or (`jpeg`) a JPEG kept as JPEG?
fn kept(r: &Rule, dims: (u32, u32), size: (f64, f64), jpeg: bool) -> bool {
    sized(r, dims, size) == dims
        && match r.codec {
            Codec::Auto(_) => true,
            Codec::Zip => !jpeg,
            Codec::Jpeg(_) => jpeg,
        }
}

/// The JPEG quality `r` compresses an image with (`jpeg`: it is a JPEG); `None`: lossless.
fn jpeg_quality(r: &Rule, jpeg: bool) -> Option<u8> {
    match r.codec {
        Codec::Jpeg(q) => Some(q),
        Codec::Auto(q) if jpeg => Some(q),
        _ => None,
    }
}

/// `img` resampled to `w` × `h` pixels as `r` says.
fn resample(img: DynamicImage, r: &Rule, (w, h): (u32, u32)) -> DynamicImage {
    if (w, h) == (img.width(), img.height()) {
        return img;
    }
    match r.downsample {
        Downsample::Average => img.thumbnail_exact(w, h),
        Downsample::Subsample => img.resize_exact(w, h, FilterType::Nearest),
        Downsample::Bicubic | Downsample::None => img.resize_exact(w, h, FilterType::CatmullRom),
    }
}

/// Encoded image `bytes` placed `size` points wide and high, as the Compression settings `c` ask:
/// resampled when its resolution is above the threshold, compressed with ZIP or JPEG, its colours
/// converted by `convert` (colour images). `interpolate` lets viewers smooth it. An image that
/// can't be decoded is kept as it is.
pub(crate) fn recode(bytes: &[u8], size: (f64, f64), c: &CompressionSettings, interpolate: bool, convert: Option<&Pixels>) -> Recoded {
    let keep = Recoded { image: None, warning: None };
    let Ok(reader) = ImageReader::new(Cursor::new(bytes)).with_guessed_format() else { return keep };
    let jpeg = reader.format() == Some(ImageFormat::Jpeg);
    let Ok(decoder) = reader.into_decoder() else { return keep };
    let dims = decoder.dimensions();
    use ExtendedColorType as T;
    let grey = matches!(decoder.original_color_type(), T::L1 | T::La1 | T::L2 | T::La2 | T::L4 | T::La4 | T::L8 | T::La8 | T::L16 | T::La16);
    let (kind, pixels) = if grey {
        // Only the pixels tell monochrome images from greyscale ones: decoded unless both keep it.
        let (g, m) = (rule(c, Kind::Gray), rule(c, Kind::Mono));
        if kept(&g, dims, size, jpeg) && kept(&m, dims, size, jpeg) && g.instead == m.instead {
            return Recoded { image: None, warning: g.instead };
        }
        let Ok(img) = DynamicImage::from_decoder(decoder) else { return keep };
        (if bilevel(&img) { Kind::Mono } else { Kind::Gray }, Some(img))
    } else {
        (Kind::Color, None)
    };
    let rule = rule(c, kind);
    let convert = convert.filter(|_| kind == Kind::Color);
    if kept(&rule, dims, size, jpeg) && convert.is_none() {
        return Recoded { image: None, warning: rule.instead };
    }
    let Some(img) = pixels.or_else(|| image::load_from_memory(bytes).ok()) else { return keep };
    let mut rgba = resample(img, &rule, sized(&rule, dims, size)).into_rgba8();
    if kind == Kind::Mono {
        // Resampling greys the edges: back to black and white.
        for p in rgba.pixels_mut() {
            let v = if p[0] >= 128 { 255 } else { 0 };
            (p[0], p[1], p[2]) = (v, v, v);
        }
    }
    if let Some(p) = convert {
        p.rgb_in_place(&mut rgba);
    }
    let (width, height) = rgba.dimensions();
    let opaque = rgba.pixels().all(|p| p[3] == 255);
    let jpeg = jpeg_quality(&rule, jpeg).filter(|_| opaque).and_then(|q| {
        // Grey pixels stay grey; colour ones go to CMYK when the output converts to CMYK.
        let grey = kind != Kind::Color || neutral(&rgba);
        let (samples, space) = match convert.filter(|_| !grey).and_then(|p| p.cmyk(&rgba)) {
            Some(cmyk) => (cmyk, ImageColorspace::Cmyk),
            None if grey => (rgba.pixels().map(|p| p[0]).collect(), ImageColorspace::Luma),
            None => (rgba.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect(), ImageColorspace::Rgb),
        };
        to_jpeg(&samples, space, width, height, q, interpolate)
    });
    let image = jpeg.or_else(|| from_rgba(rgba.as_raw(), width, height, interpolate, convert));
    Recoded { image, warning: rule.instead }
}

/// [`recode`] of CMYK image `blob` ([`ImageBlob::cmyk`]): a JPEG already as asked is kept as it
/// is when its numbers are (`convert` is `None`); otherwise its ink amounts are resampled,
/// converted by `convert` and compressed again.
pub(crate) fn recode_cmyk(blob: &ImageBlob, size: (f64, f64), c: &CompressionSettings, interpolate: bool, convert: Option<&CmykPixels>) -> Recoded {
    let rule = rule(c, Kind::Color);
    if convert.is_none()
        && let Some(j) = blob.source_jpeg.as_ref().filter(|j| j.decoded_key == blob.content_key())
    {
        let dims = ImageReader::new(Cursor::new(blob.bytes.as_slice())).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok());
        if dims.is_some_and(|d| kept(&rule, d, size, true))
            && let Ok(image) = Image::from_pdf_cmyk_jpeg(j.bytes.as_ref().clone().into(), interpolate, j.invert)
        {
            return Recoded { image: Some(image), warning: rule.instead };
        }
    }
    let jpeg = jpeg_components(&blob.bytes).is_some();
    if jpeg && convert.is_none() {
        let dims = ImageReader::new(Cursor::new(blob.bytes.as_slice())).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok());
        if dims.is_some_and(|d| kept(&rule, d, size, true)) {
            return Recoded { image: None, warning: rule.instead };
        }
    }
    let Some(inks) = blob.cmyk() else {
        return Recoded { image: None, warning: Some(if jpeg { CMYK_KEPT } else { CMYK_RGB }) };
    };
    let dims = (inks.width, inks.height);
    let (width, height) = sized(&rule, dims, size);
    // The four inks resample as the four channels of an RGBA image do.
    let inks = match image::RgbaImage::from_raw(inks.width, inks.height, inks.data) {
        Some(img) => resample(DynamicImage::ImageRgba8(img), &rule, (width, height)).into_rgba8().into_raw(),
        None => return Recoded { image: None, warning: Some(CMYK_RGB) },
    };
    let (samples, space) = match convert {
        Some(p) => p.convert(&inks),
        None => (inks, ImageColorspace::Cmyk),
    };
    let image = jpeg_quality(&rule, jpeg).and_then(|q| to_jpeg(&samples, space, width, height, q, interpolate));
    let image = image.or_else(|| lossless(samples, space, width, height, interpolate));
    Recoded { image, warning: rule.instead }
}

/// Opaque colour `samples` (grey, RGB, or CMYK ink amounts, as `space` says) as a JPEG at
/// `quality`. CMYK JPEGs hold Adobe's inverted samples, which the PDF writer's Decode array
/// undoes.
fn to_jpeg(samples: &[u8], space: ImageColorspace, width: u32, height: u32, quality: u8, interpolate: bool) -> Option<Image> {
    let mut out = Vec::new();
    match space {
        ImageColorspace::Cmyk => {
            let (w, h) = (u16::try_from(width).ok()?, u16::try_from(height).ok()?);
            jpeg_encoder::Encoder::new(&mut out, quality).encode(samples, w, h, jpeg_encoder::ColorType::Cmyk).ok()?;
        }
        ImageColorspace::Rgb | ImageColorspace::Luma => {
            let t = if space == ImageColorspace::Rgb { ExtendedColorType::Rgb8 } else { ExtendedColorType::L8 };
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode(samples, width, height, t).ok()?;
        }
    }
    Image::from_jpeg(out.into(), interpolate).ok()
}
