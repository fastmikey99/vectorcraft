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
fn spot_names(d: &mut tiff::decoder::Decoder<Cursor<&[u8]>>, width: u32, height: u32) -> Vec<String> {
    let mut names: Vec<String> = vec![];
    while d.more_images() {
        if names.len() == MAX_SPOTS || d.next_image().is_err() {
            return vec![];
        }
        match page_name(d, width, height) {
            Some(n) if !names.contains(&n) => names.push(n),
            _ => return vec![],
        }
    }
    names
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
                Ok((w, h)) => spot_names(&mut d, w, h),
                Err(_) => vec![],
            },
            _ => vec![],
        }
    }

    /// The spot ink amounts of a CMYK image with spot channels; `None` for other images, and ones
    /// that can't be decoded or are larger than [`super::MAX_CMYK_PIXELS`].
    pub fn spot_inks(&self) -> Option<SpotInks> {
        let (kind, mut d) = tiff(&self.bytes)?;
        if !matches!(kind, tiff::ColorType::CMYK(8 | 16)) {
            return None;
        }
        let (w, h) = d.dimensions().ok()?;
        let n = usize::try_from(pixels(w, h)?).ok()?;
        let names = spot_names(&mut d, w, h);
        if names.is_empty() {
            return None;
        }
        // Read again from the first image: the names were read by walking past every page.
        let (_, mut d) = tiff(&self.bytes)?;
        let mut data = vec![0u8; n.checked_mul(names.len())?];
        for i in 0..names.len() {
            d.next_image().ok()?;
            let plane = read_samples(&mut d, n, 1)?;
            for (o, v) in data.iter_mut().skip(i).step_by(names.len()).zip(plane) {
                *o = v;
            }
        }
        SpotInks::new(w, h, names, data)
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
        assert_eq!(blob.spot_inks().unwrap(), spots);
        assert_eq!(spots.channel(1).collect::<Vec<_>>(), [0, 64]);
    }

    #[test]
    fn plain_cmyk_images_have_no_spot_channels() {
        let (inks, _) = sample();
        let blob = ImageBlob::cmyk_tiff(&inks).unwrap();
        assert!(blob.spot_names().is_empty());
        assert!(blob.spot_inks().is_none());
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
