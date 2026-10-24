//! Spot ink channels of CMYK images: a PDF image in a DeviceN or Separation space whose inks
//! include spot inks keeps each one as a channel of its own, printed on that ink's plate.
//!
//! They are stored in the image's CMYK TIFF ([`ImageBlob::cmyk_spot_tiff`]): its first image is
//! the process inks, as [`ImageBlob::cmyk_tiff`] writes them, and each spot channel follows as an
//! 8-bit grayscale image of the same size whose samples are ink amounts (0 is no ink), named by a
//! private tag ([`NAMES_TAG`]: the spot swatch's name in UTF-8, ended by a NUL). Readers that
//! don't know the tag see a CMYK image (and further pages); [`ImageBlob::cmyk`] reads the process
//! inks alone.

use std::io::Cursor;

use crate::ImageBlob;

use super::{Inks, pixels, read_samples, tiff};

/// The private TIFF tag (in the reusable range, 65000–65535) holding the spot channels' swatch
/// names.
pub const NAMES_TAG: u16 = 65_421;

/// Most spot channels of an image: with the four process inks, the 32 colorants a PDF DeviceN
/// space may have.
pub const MAX_SPOTS: usize = 28;
/// Working process/spot bytes for raster generation, before renderer scratch allocation.
pub const MAX_INK_BYTES: u64 = if cfg!(target_arch = "wasm32") { 1 << 28 } else { 1_000_000_000 };

/// Decode allowance separate from raster-generation scratch: native retains support for the
/// former 2^26-pixel, 32-colorant images (including 40 MP with 28 spots). WASM stays bounded.
pub const MAX_SPOT_DECODE_BYTES: u64 = if cfg!(target_arch = "wasm32") { MAX_INK_BYTES } else { 1 << 31 };

/// A spot-bearing image must never be treated as process-only when its channels cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum SpotDecodeError {
    #[error("spot image requires {required} decoded ink bytes, exceeding the {limit}-byte limit")]
    Limit { required: u64, limit: u64 },
    #[error("spot image has invalid or unreadable channels")]
    Invalid,
    #[error("not enough memory to decode spot image channels")]
    Allocation,
}

fn sample_count(width: u32, height: u32, channels: usize, limit: u64) -> Result<usize, SpotDecodeError> {
    let n = u64::from(width) * u64::from(height);
    let required = n.checked_mul(channels as u64 + 4).ok_or(SpotDecodeError::Invalid)?;
    if required > limit {
        return Err(SpotDecodeError::Limit { required, limit });
    }
    pixels(width, height).ok_or(SpotDecodeError::Invalid)?;
    usize::try_from(n).map_err(|_| SpotDecodeError::Invalid)
}

/// The spot ink amounts of an image, a byte a channel, pixel by pixel: `names.len()` bytes a pixel
/// (0 is no ink), each channel printing with the spot swatch of that name.
#[derive(Clone, Debug, PartialEq)]
pub struct SpotInks {
    pub width: u32,
    pub height: u32,
    pub names: Vec<String>,
    pub data: Vec<u8>,
}

impl SpotInks {
    /// `data` as a `width` × `height` image of channels `names`; `None` when it doesn't hold that
    /// many samples, has no channel or more than [`MAX_SPOTS`], a name is empty or holds a NUL,
    /// two names are the same, or the image is empty or larger than [`super::MAX_CMYK_PIXELS`].
    pub fn new(width: u32, height: u32, names: Vec<String>, data: Vec<u8>) -> Option<Self> {
        let n = pixels(width, height)?;
        let named = names.iter().all(|s| !s.is_empty() && !s.contains('\0'));
        let distinct = names.iter().enumerate().all(|(i, s)| !names.iter().take(i).any(|t| t == s));
        let sized = (1..=MAX_SPOTS).contains(&names.len()) && data.len() as u64 == n * names.len() as u64;
        (named && distinct && sized).then_some(Self { width, height, names, data })
    }

    /// The amounts of channel `i`, pixel by pixel.
    pub fn channel(&self, i: usize) -> impl Iterator<Item = u8> + '_ {
        let per = self.names.len().max(1);
        self.data.iter().skip(i).step_by(per).copied()
    }
}

/// The name [`NAMES_TAG`] of the image `d` is at holds, when it is a spot channel of a `width` ×
/// `height` image.
fn page_name(d: &mut tiff::decoder::Decoder<Cursor<&[u8]>>, width: u32, height: u32) -> Option<String> {
    if d.colortype().ok()? != tiff::ColorType::Gray(8) || d.dimensions().ok()? != (width, height) {
        return None;
    }
    let bytes = d.find_tag_unsigned_vec::<u8>(tiff::tags::Tag::Unknown(NAMES_TAG)).ok()??;
    let name = String::from_utf8(bytes.strip_suffix(&[0])?.to_vec()).ok()?;
    (!name.is_empty() && !name.contains('\0')).then_some(name)
}

/// The spot channel names of the CMYK TIFF `d` reads (at its first image, `width` × `height`
/// pixels): every following image that is one, in order, up to [`MAX_SPOTS`]. None when a following
/// image isn't one, or two have one name.
fn spot_names(d: &mut tiff::decoder::Decoder<Cursor<&[u8]>>, width: u32, height: u32) -> Result<Vec<String>, SpotDecodeError> {
    let mut names: Vec<String> = vec![];
    while d.more_images() {
        d.next_image().map_err(|_| SpotDecodeError::Invalid)?;
        if !is_spot_page(d)? {
            continue;
        }
        match page_name(d, width, height) {
            Some(n) if names.len() < MAX_SPOTS && !names.contains(&n) => names.push(n),
            _ => return Err(SpotDecodeError::Invalid),
        }
    }
    Ok(names)
}

fn is_spot_page(d: &mut tiff::decoder::Decoder<Cursor<&[u8]>>) -> Result<bool, SpotDecodeError> {
    // Ordinary multipage TIFFs have no private tag. A present but invalid tag is
    // still a declared spot channel, and must fail rather than silently lose ink.
    d.find_tag(tiff::tags::Tag::Unknown(NAMES_TAG)).map(|tag| tag.is_some()).map_err(|_| SpotDecodeError::Invalid)
}

impl ImageBlob {
    /// A CMYK TIFF of process inks `inks` with spot channels `spots` (Deflate-compressed); `None`
    /// when their sizes differ or it can't be written.
    pub fn cmyk_spot_tiff(inks: &Inks, spots: &SpotInks) -> Option<Self> {
        use tiff::encoder::{Compression, TiffEncoder, colortype::CMYK8, colortype::Gray8, compression::DeflateLevel};
        use tiff::tags::Tag;
        if (inks.width, inks.height) != (spots.width, spots.height) {
            return None;
        }
        let mut out = Cursor::new(Vec::new());
        let mut enc = TiffEncoder::new(&mut out).ok()?.with_compression(Compression::Deflate(DeflateLevel::Balanced));
        enc.new_image::<CMYK8>(inks.width, inks.height).ok()?.write_data(&inks.data).ok()?;
        for (i, name) in spots.names.iter().enumerate() {
            let mut img = enc.new_image::<Gray8>(inks.width, inks.height).ok()?;
            let mut tag = name.as_bytes().to_vec();
            tag.push(0);
            img.encoder().write_tag(Tag::Unknown(NAMES_TAG), &tag[..]).ok()?;
            img.write_data(&spots.channel(i).collect::<Vec<_>>()).ok()?;
        }
        Some(Self::new("image/tiff", out.into_inner()))
    }

    /// The spot channel names of a CMYK image ([`SpotInks::names`]); none for other images. Reads
    /// only its headers.
    pub fn spot_names(&self) -> Vec<String> {
        match tiff(&self.bytes) {
            Some((tiff::ColorType::CMYK(8 | 16), mut d)) => match d.dimensions() {
                Ok((w, h)) => spot_names(&mut d, w, h).unwrap_or_default(),
                Err(_) => vec![],
            },
            _ => vec![],
        }
    }

    /// Spot channels of a CMYK image. `Ok(None)` means no spot channels; invalid channels,
    /// decode limits and allocation failures are errors, never permission to drop the spots.
    pub fn spot_inks(&self) -> Result<Option<SpotInks>, SpotDecodeError> {
        self.spot_inks_with_limit(MAX_SPOT_DECODE_BYTES)
    }

    fn spot_inks_with_limit(&self, limit: u64) -> Result<Option<SpotInks>, SpotDecodeError> {
        let Some((kind, mut d)) = tiff(&self.bytes) else { return Ok(None) };
        if !matches!(kind, tiff::ColorType::CMYK(8 | 16)) || !d.more_images() {
            return Ok(None);
        }
        let (w, h) = d.dimensions().map_err(|_| SpotDecodeError::Invalid)?;
        let names = spot_names(&mut d, w, h)?;
        if names.is_empty() {
            return Ok(None);
        }
        let n = sample_count(w, h, names.len(), limit)?;
        // Read again from the first image: the names were read by walking past every page.
        let (_, mut d) = tiff(&self.bytes).ok_or(SpotDecodeError::Invalid)?;
        let len = n.checked_mul(names.len()).ok_or(SpotDecodeError::Invalid)?;
        let mut data = Vec::new();
        data.try_reserve_exact(len).map_err(|_| SpotDecodeError::Allocation)?;
        data.resize(len, 0);
        let mut i = 0;
        while d.more_images() {
            d.next_image().map_err(|_| SpotDecodeError::Invalid)?;
            if !is_spot_page(&mut d)? {
                continue;
            }
            let plane = read_samples(&mut d, n, 1).ok_or(SpotDecodeError::Invalid)?;
            for (o, v) in data.iter_mut().skip(i).step_by(names.len()).zip(plane) {
                *o = v;
            }
            i += 1;
        }
        SpotInks::new(w, h, names, data).map(Some).ok_or(SpotDecodeError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Inks, SpotInks) {
        let inks = Inks::new(2, 1, vec![10, 20, 30, 40, 0, 0, 0, 255]).unwrap();
        let spots = SpotInks::new(2, 1, vec!["Ink A".into(), "Encre é".into()], vec![255, 0, 128, 64]).unwrap();
        (inks, spots)
    }

    #[test]
    fn spot_channels_round_trip_beside_the_process_inks() {
        let (inks, spots) = sample();
        let blob = ImageBlob::cmyk_spot_tiff(&inks, &spots).unwrap();
        assert!(blob.is_cmyk(), "still a CMYK image to everything that reads its process inks");
        assert_eq!(blob.cmyk().unwrap(), inks);
        assert_eq!(blob.spot_names(), spots.names);
        assert_eq!(blob.spot_inks().unwrap().unwrap(), spots);
        assert_eq!(spots.channel(1).collect::<Vec<_>>(), [0, 64]);
    }

    #[test]
    fn plain_cmyk_images_have_no_spot_channels() {
        let (inks, _) = sample();
        let blob = ImageBlob::cmyk_tiff(&inks).unwrap();
        assert!(blob.spot_names().is_empty());
        assert!(blob.spot_inks().unwrap().is_none());
    }

    fn multipage(tags: &[Option<&[u8]>]) -> ImageBlob {
        use tiff::encoder::{TiffEncoder, colortype::CMYK8, colortype::Gray8};
        let mut out = Cursor::new(Vec::new());
        let mut enc = TiffEncoder::new(&mut out).unwrap();
        enc.new_image::<CMYK8>(2, 1).unwrap().write_data(&sample().0.data).unwrap();
        for tag in tags {
            // Untagged pages need not even match the first page's size or colour space.
            if let Some(tag) = tag {
                let mut img = enc.new_image::<Gray8>(2, 1).unwrap();
                img.encoder().write_tag(tiff::tags::Tag::Unknown(NAMES_TAG), *tag).unwrap();
                img.write_data(&[37, 219]).unwrap();
            } else {
                enc.new_image::<CMYK8>(1, 1).unwrap().write_data(&[1, 2, 3, 4]).unwrap();
                enc.new_image::<Gray8>(2, 1).unwrap().write_data(&[8, 9]).unwrap();
            }
        }
        ImageBlob::new("image/tiff", out.into_inner())
    }

    #[test]
    fn ordinary_multipage_cmyk_tiffs_ignore_plain_extra_pages() {
        let blob = multipage(&[None; MAX_SPOTS + 1]);
        assert_eq!(blob.cmyk().unwrap(), sample().0);
        assert!(blob.spot_names().is_empty());
        assert_eq!(blob.spot_inks_with_limit(0).unwrap(), None);
    }

    #[test]
    fn spot_pages_decode_past_untagged_pages_without_losing_ink() {
        let blob = multipage(&[None, Some(b"A\0"), None, Some(b"B\0"), None]);
        assert_eq!(blob.spot_names(), ["A", "B"]);
        assert_eq!(blob.spot_inks().unwrap().unwrap(), SpotInks::new(2, 1, vec!["A".into(), "B".into()], vec![37, 37, 219, 219]).unwrap());
        for tags in [vec![None, Some(&b"invalid"[..])], vec![Some(&b"A\0"[..]), None, Some(&b"A\0"[..])]] {
            assert!(matches!(multipage(&tags).spot_inks(), Err(SpotDecodeError::Invalid)));
        }
    }

    #[test]
    fn spot_limit_failure_is_an_error_not_absent_ink() {
        let (inks, spots) = sample();
        let blob = ImageBlob::cmyk_spot_tiff(&inks, &spots).unwrap();
        assert!(matches!(blob.spot_inks_with_limit(11), Err(SpotDecodeError::Limit { required: 12, limit: 11 })));
        assert_eq!(blob.spot_inks_with_limit(12).unwrap().unwrap(), spots);
        let plain = ImageBlob::cmyk_tiff(&inks).unwrap();
        assert_eq!(plain.spot_inks_with_limit(0).unwrap(), None);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn native_spot_decode_keeps_previously_supported_40mp_28_spot_images() {
        assert_eq!(sample_count(8_000, 5_000, 28, MAX_SPOT_DECODE_BYTES).unwrap(), 40_000_000);
        assert_eq!(sample_count(8192, 8192, 28, MAX_SPOT_DECODE_BYTES).unwrap(), 1 << 26);
        assert!(matches!(sample_count(10_000, 10_000, 28, MAX_SPOT_DECODE_BYTES), Err(SpotDecodeError::Limit { .. })));
    }

    #[test]
    fn bad_spot_channels_are_refused() {
        assert!(SpotInks::new(1, 1, vec![], vec![]).is_none());
        assert!(SpotInks::new(1, 1, vec!["A".into(), "A".into()], vec![1, 2]).is_none());
        assert!(SpotInks::new(1, 1, vec!["".into()], vec![1]).is_none());
        assert!(SpotInks::new(1, 1, vec!["A".into()], vec![1, 2]).is_none());
        assert!(SpotInks::new(1, 1, (0..=MAX_SPOTS).map(|i| i.to_string()).collect(), vec![0; MAX_SPOTS + 1]).is_none());
    }
}
