//! Streams looked at before the PDF reader (hayro) decodes them. It inflates Flate and LZW data
//! with no ceiling, so a few megabytes can ask for gigabytes (#1700), and it applies a predictor
//! before checking its parameters: `Columns 0` divides by zero, a huge `Columns` allocates a row
//! that large whatever the stream holds (#1701). A file with such a stream is refused instead.

use std::borrow::Cow;
use std::io::Read;

use craft_pdf::syntax::content::TypedIter;
use craft_pdf::syntax::content::ops::TypedInstruction;
use craft_pdf::syntax::object::{Array, Dict, FromBytes, MaybeRef, Name, Object, Stream};

/// The most a stream may decode to: 256 MB, the pixels of the largest image the app reads (64
/// million, 4 bytes each).
pub(crate) const MAX_DECODED: usize = 256 << 20;

/// The most an image XObject may decode to when its own size asks for more than
/// [`MAX_DECODED`]: 1 GB (an A0 sheet scanned at 300 ppi is 418 MB in RGB, 557 MB in CMYK).
const MAX_IMAGE_DECODED: usize = 1 << 30;

/// The most a stream used as content (a page's contents, a form, a tiling pattern, a Type 3
/// glyph, an appearance stream or a soft mask's group) may decode to when it is large already:
/// 512 MB, at most [`MAX_RATIO`] times its encoded size. That is the reader's content limit and
/// the interpreter's budget for the content one page holds, so what passes here is read whole.
/// Real drawings compress 3–10 times (a 22 × 180 in Inkscape page is 128 MB that decodes to
/// 391 MB); a zip bomb expands hundreds of times, and stays at [`MAX_DECODED`].
pub(crate) const MAX_CONTENT_DECODED: usize = craft_pdf::syntax::MAX_DECODED_CONTENT;

/// The most a content stream past [`MAX_DECODED`] may expand: 32 times its encoded size.
const MAX_RATIO: usize = 32;

/// The longest row a predictor may have, in bytes.
const MAX_ROW: usize = 1 << 24;

/// A stream that decodes to more than its [`budget`].
struct TooBig;

/// The filters of the stream with dictionary `dict`, by name (`Filter` or `F`: a name or an array).
fn filters<'a>(dict: &Dict<'a>) -> Vec<Name<'a>> {
    [&b"Filter"[..], b"F"]
        .into_iter()
        .find_map(|key| dict.get::<Name<'a>>(key).map(|n| vec![n]).or_else(|| dict.get::<Array<'a>>(key).map(|a| a.iter::<Name<'a>>().collect())))
        .unwrap_or_default()
}

/// Every decode parameter dictionary of the stream with dictionary `dict`, as the reader may take
/// them (`DecodeParms` or `DP`: a dictionary, or an array with one for each filter).
fn params<'a>(dict: &Dict<'a>) -> Vec<Dict<'a>> {
    let mut all = vec![];
    for key in [&b"DecodeParms"[..], b"DP"] {
        all.extend(dict.get::<Dict<'a>>(key));
        all.extend(dict.get::<Array<'a>>(key).into_iter().flat_map(|a| a.iter::<Object<'a>>().filter_map(|o| o.into_dict()).collect::<Vec<_>>()));
    }
    all
}

/// Would the reader's predictor fail on `params`? It reads `Colors` and `BitsPerComponent` as
/// bytes and `Columns` as a size, multiplies the first two as bytes, divides by the length of a row
/// and allocates one before it checks anything.
fn bad_predictor(params: &Dict<'_>) -> bool {
    if params.get::<u8>(b"Predictor").unwrap_or(1) == 1 {
        return false;
    }
    let colors = usize::from(params.get::<u8>(b"Colors").unwrap_or(1));
    let bits = usize::from(params.get::<u8>(b"BitsPerComponent").unwrap_or(8));
    let columns = params.get::<usize>(b"Columns").unwrap_or(1);
    let pixel = colors * bits;
    let row = columns.checked_mul(pixel).map(|b| b.div_ceil(8));
    !(matches!(bits, 1 | 2 | 4 | 8 | 16) && colors > 0 && pixel <= 255 && columns > 0 && row.is_some_and(|r| r <= MAX_ROW))
}

/// `data` inflated, up to one byte past `max` (zlib, or raw deflate as the reader also takes);
/// `None` when it is damaged before that.
fn inflate(data: &[u8], max: usize) -> Option<Vec<u8>> {
    let read = |mut r: Box<dyn Read + '_>| {
        let mut out = vec![];
        // (Damaged data: what came out before the damage still counts.)
        let done = r.by_ref().take(max as u64 + 1).read_to_end(&mut out).is_ok();
        (done || out.len() > max).then_some(out)
    };
    read(Box::new(flate2::read::ZlibDecoder::new(data))).or_else(|| read(Box::new(flate2::read::DeflateDecoder::new(data))))
}

/// `data` decoded from LZW, up to one byte past `max`. `early`: the code length grows one code
/// early (`EarlyChange`, 1 by default).
fn lzw(data: &[u8], early: bool, max: usize) -> Option<Vec<u8>> {
    use weezl::{BitOrder, LzwStatus, decode::Decoder};
    let mut decoder = if early { Decoder::with_tiff_size_switch(BitOrder::Msb, 8) } else { Decoder::new(BitOrder::Msb, 8) };
    let (mut out, mut chunk, mut input) = (vec![], vec![0; 1 << 16], data);
    while out.len() <= max {
        let r = decoder.decode_bytes(input, &mut chunk);
        input = input.get(r.consumed_in..).unwrap_or_default();
        out.extend_from_slice(chunk.get(..r.consumed_out).unwrap_or_default());
        match r.status {
            Ok(LzwStatus::Ok) => {}
            Ok(LzwStatus::Done | LzwStatus::NoProgress) => break,
            Err(_) => return (out.len() > max).then_some(out),
        }
    }
    Some(out)
}

/// `data` decoded from run-length encoding, up to one byte past `max`.
fn run_length(data: &[u8], max: usize) -> Vec<u8> {
    let (mut out, mut at) = (vec![], 0);
    while let Some(&n) = data.get(at)
        && n != 128
        && out.len() <= max
    {
        if n < 128 {
            let take = usize::from(n) + 1;
            out.extend_from_slice(data.get(at + 1..).unwrap_or_default().get(..take).unwrap_or_default());
            at += 1 + take;
        } else {
            out.extend(std::iter::repeat_n(data.get(at + 1).copied().unwrap_or(0), 257 - usize::from(n)));
            at += 2;
        }
    }
    out
}

/// `data` decoded from hexadecimal digits (white space skipped, up to `>`).
fn hex(data: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = data.iter().take_while(|c| **c != b'>').filter_map(|c| char::from(*c).to_digit(16)).map(|d| d as u8).collect();
    digits.chunks(2).map(|p| (p.first().copied().unwrap_or(0) << 4) | p.get(1).copied().unwrap_or(0)).collect()
}

/// `data` decoded from base-85 (white space skipped, `z` for four zeros, up to `~>`).
fn base85(data: &[u8]) -> Vec<u8> {
    let (mut out, mut group) = (vec![], vec![]);
    let flush = |group: &mut Vec<u32>, out: &mut Vec<u8>| {
        let n = group.len();
        if n > 1 {
            group.resize(5, 84);
            let v = group.iter().fold(0u32, |v, d| v.wrapping_mul(85).wrapping_add(*d));
            out.extend_from_slice(v.to_be_bytes().get(..n - 1).unwrap_or_default());
        }
        group.clear();
    };
    for &c in data.iter().take_while(|c| **c != b'~') {
        match c {
            b'z' if group.is_empty() => out.extend([0; 4]),
            b'!'..=b'u' => {
                group.push(u32::from(c - b'!'));
                if group.len() == 5 {
                    flush(&mut group, &mut out);
                }
            }
            _ => {}
        }
    }
    flush(&mut group, &mut out);
    out
}

/// The most data of `len` bytes can grow to through `filters`: 1,032 times through Flate (its
/// largest ratio), 3,641 through LZW (4,096 bytes from a 9-bit code), 64 through run-length.
fn most_after(len: usize, filters: &[Name<'_>]) -> u128 {
    filters.iter().fold(len as u128, |n, f| {
        n.saturating_mul(match f.as_ref() {
            b"FlateDecode" | b"Fl" => 1032,
            b"LZWDecode" | b"LZW" => 3641,
            b"RunLengthDecode" | b"RL" => 64,
            _ => 1,
        })
    })
}

/// The number of colour components of the image with dictionary `dict`, from its colour space
/// (4 when it can't be told).
fn components(dict: &Dict<'_>) -> u64 {
    let named = |n: &[u8]| match n {
        b"DeviceGray" | b"G" | b"CalGray" | b"Indexed" | b"I" | b"Separation" => 1,
        b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => 3,
        _ => 4,
    };
    if let Some(n) = dict.get::<Name<'_>>(b"ColorSpace") {
        return named(n.as_ref());
    }
    let Some(a) = dict.get::<Array<'_>>(b"ColorSpace") else { return 4 };
    let mut it = a.flex_iter();
    let any = |n: Option<usize>| n.filter(|n| (1..=32).contains(n)).map_or(4, |n| n as u64);
    match it.next::<Name<'_>>() {
        Some(n) if n.as_ref() == b"ICCBased" => any(it.next::<Stream<'_>>().and_then(|s| s.dict().get::<usize>(b"N"))),
        Some(n) if n.as_ref() == b"DeviceN" => any(it.next::<Array<'_>>().map(|names| names.iter::<Object<'_>>().count())),
        Some(n) => named(n.as_ref()),
        None => 4,
    }
}

/// The most the stream with dictionary `dict` may decode to: [`MAX_DECODED`], or for an image
/// XObject the size its width, height, components and bits declare (each row with a predictor's
/// tag byte), up to [`MAX_IMAGE_DECODED`]: an image's size is known before it is decoded. A
/// stream used as `content`, `encoded` bytes long, may decode to [`MAX_RATIO`] times that, up to
/// [`MAX_CONTENT_DECODED`].
fn budget(dict: &Dict<'_>, encoded: usize, content: bool) -> usize {
    if dict.get::<Name<'_>>(b"Subtype").is_none_or(|s| s.as_ref() != b"Image") {
        if content {
            return encoded.saturating_mul(MAX_RATIO).clamp(MAX_DECODED, MAX_CONTENT_DECODED);
        }
        return MAX_DECODED;
    }
    let side = |key: &[u8]| dict.get::<u32>(key).map_or(0, u64::from);
    let mask = dict.get::<bool>(b"ImageMask") == Some(true);
    let bits = if mask { 1 } else { dict.get::<u8>(b"BitsPerComponent").map_or(8, |b| u64::from(b.min(16))) };
    let channels = if mask { 1 } else { components(dict) };
    let row = side(b"Width").saturating_mul(channels).saturating_mul(bits).div_ceil(8).saturating_add(1);
    let declared = usize::try_from(side(b"Height").saturating_mul(row)).unwrap_or(usize::MAX);
    declared.clamp(MAX_DECODED, MAX_IMAGE_DECODED)
}

/// `data` (of the stream with dictionary `dict`) through its filters, as far as this reads them:
/// Flate, LZW, run-length and the ASCII encodings. Not decoded when it can't decode past its
/// [`budget`] and `keep` doesn't ask for it, or when an image codec or damaged data comes first
/// (an image's own decoder knows its size). `content`: the stream is used as content.
fn decode(data: &[u8], dict: &Dict<'_>, keep: bool, content: bool) -> Result<Option<Vec<u8>>, TooBig> {
    let filters = filters(dict);
    let max = budget(dict, data.len(), content);
    if !keep && most_after(data.len(), &filters) <= max as u128 {
        return Ok(None);
    }
    let params = params(dict);
    let mut data = Cow::Borrowed(data);
    for (i, f) in filters.iter().enumerate() {
        let early = params.get(i).and_then(|p| p.get::<u8>(b"EarlyChange")).is_none_or(|e| e != 0);
        let out = match f.as_ref() {
            b"FlateDecode" | b"Fl" => inflate(&data, max),
            b"LZWDecode" | b"LZW" => lzw(&data, early, max),
            b"RunLengthDecode" | b"RL" => Some(run_length(&data, max)),
            b"ASCIIHexDecode" | b"AHx" => Some(hex(&data)),
            b"ASCII85Decode" | b"A85" => Some(base85(&data)),
            _ => None,
        };
        match out {
            Some(out) if out.len() > max => return Err(TooBig),
            Some(out) => data = Cow::Owned(out),
            None => return Ok(None),
        }
    }
    Ok(Some(data.into_owned()))
}

/// May the stream with dictionary `dict` be content (of a page, form, pattern or glyph), which can
/// hold inline images? Not an image, font, profile, function, metadata or object stream.
fn maybe_content(dict: &Dict<'_>) -> bool {
    let named = |key: &[u8], ok: &[&[u8]]| dict.get::<Name<'_>>(key).is_none_or(|n| ok.contains(&n.as_ref()));
    named(b"Subtype", &[b"Form"])
        && named(b"Type", &[b"XObject", b"Pattern"])
        && ![&b"Length1"[..], b"Length2", b"Length3", b"FunctionType", b"N"].iter().any(|k| dict.contains_key(k))
}

/// Is the stream with dictionary `dict` content by what it is: a form XObject (soft masks' groups
/// and appearance streams usually are) or a tiling pattern? Content used from elsewhere (a page's
/// contents, a Type 3 glyph) is known by where it is used ([`crate::pages`]).
fn content_by_kind(dict: &Dict<'_>) -> bool {
    dict.get::<Name<'_>>(b"Subtype").is_some_and(|s| s.as_ref() == b"Form") || dict.get::<u8>(b"PatternType") == Some(1)
}

/// Would the reader fail on `stream` (or on an inline image in it, when it may be content): a
/// predictor it can't apply, or more than its [`budget`] decoded? `used_as_content`: the file uses
/// it as a page's contents, a Type 3 glyph, an appearance stream or a soft mask's group. Only
/// content (that, or a form or tiling pattern) may decode past [`MAX_DECODED`]; any other stream
/// (a colour table, a shading, a function, a font) may not, whatever its dictionary leaves out.
pub(crate) fn unsafe_stream(stream: &Stream<'_>, used_as_content: bool) -> bool {
    unsafe_data(stream, true, used_as_content)
}

/// [`unsafe_stream`], looking for inline images in it when `inside` (not in an inline image's own
/// data: it would be read again for each inline image a hostile one nests).
fn unsafe_data(stream: &Stream<'_>, inside: bool, used_as_content: bool) -> bool {
    let dict = stream.dict();
    if params(dict).iter().any(bad_predictor) {
        return true;
    }
    let content = inside && (used_as_content || content_by_kind(dict));
    // Looked through for inline images wherever it might be drawn as content.
    let scan = content || (inside && maybe_content(dict));
    match decode(&stream.raw_data(), dict, scan, content) {
        Err(TooBig) => true,
        Ok(Some(data)) if scan && data.windows(2).any(|w| w == b"ID") => {
            let mut ops = TypedIter::new(&data);
            while let Some(op) = ops.next() {
                if let TypedInstruction::InlineImage(image) = op
                    && unsafe_data(image.0, false, false)
                {
                    return true;
                }
            }
            false
        }
        Ok(_) => false,
    }
}

/// Would the reader fail on a stream it decodes while it opens `pdf`, before anything else can
/// look at it: an object stream or a cross-reference stream (read here from the file's text)?
/// Their decode parameters must be in their dictionary, as the reader can't be asked for others
/// yet. Encrypted object streams are looked at once the file is open (their data is ciphered
/// here).
pub(crate) fn unsafe_file_streams(pdf: &[u8]) -> bool {
    let find = |from: usize, what: &[u8]| pdf.get(from..).and_then(|rest| rest.windows(what.len()).position(|w| w == what)).map(|at| from + at);
    let mut from = 0;
    while let Some(at) = find(from, b"stream") {
        from = at + 6;
        if at >= 3 && pdf.get(at - 3..at) == Some(b"end") {
            continue;
        }
        // The dictionary before it: back from its `>>` to the `<<` that opens it.
        let Some(close) = pdf.get(..at).and_then(|head| head.iter().rposition(|c| !c.is_ascii_whitespace())).map(|i| i + 1) else { continue };
        if close < 2 || pdf.get(close - 2..close) != Some(b">>") {
            continue;
        }
        let (mut depth, mut open, mut i) = (0usize, None, close);
        while i >= 2 && close - i < 1 << 16 {
            match pdf.get(i - 2..i) {
                Some(b">>") => (depth, i) = (depth + 1, i - 2),
                Some(b"<<") => {
                    depth -= 1;
                    i -= 2;
                    if depth == 0 {
                        open = Some(i);
                        break;
                    }
                }
                _ => i -= 1,
            }
        }
        let Some(dict) = open.and_then(|o| pdf.get(o..close)).and_then(Dict::from_bytes) else { continue };
        if !dict.get::<Name<'_>>(b"Type").is_some_and(|t| matches!(t.as_ref(), b"ObjStm" | b"XRef")) {
            continue;
        }
        let indirect = [&b"DecodeParms"[..], b"DP"].iter().any(|k| matches!(dict.get_raw::<Object<'_>>(k), Some(MaybeRef::Ref(_))));
        if indirect || params(&dict).iter().any(bad_predictor) {
            return true;
        }
        let start = from
            + usize::from(pdf.get(from) == Some(&b'\r'))
            + usize::from(pdf.get(from + usize::from(pdf.get(from) == Some(&b'\r'))) == Some(&b'\n'));
        let end = dict
            .get::<usize>(b"Length")
            .and_then(|n| start.checked_add(n))
            .filter(|e| *e <= pdf.len())
            .or_else(|| find(start, b"endstream"))
            .unwrap_or(pdf.len());
        if matches!(decode(pdf.get(start..end).unwrap_or_default(), &dict, false, false), Err(TooBig)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(text: &str) -> Dict<'_> {
        Dict::from_bytes(text.as_bytes()).unwrap()
    }

    #[test]
    fn predictors_the_reader_can_apply_pass() {
        for ok in [
            "<< >>",
            "<< /Predictor 1 /Columns 0 >>",
            "<< /Predictor 12 /Columns 5 >>",
            "<< /Predictor 2 /Colors 3 /BitsPerComponent 16 /Columns 1000 >>",
        ] {
            assert!(!bad_predictor(&dict(ok)), "{ok}");
        }
        for bad in [
            "<< /Predictor 2 /Columns 0 >>",
            "<< /Predictor 12 /Colors 0 >>",
            "<< /Predictor 2 /BitsPerComponent 0 >>",
            "<< /Predictor 2 /BitsPerComponent 3 >>",
            "<< /Predictor 12 /Colors 32 /BitsPerComponent 16 >>",
            "<< /Predictor 15 /Columns 99999999999 >>",
            "<< /Predictor 10 /Columns 18446744073709551615 /Colors 4 >>",
        ] {
            assert!(bad_predictor(&dict(bad)), "{bad}");
        }
    }

    #[test]
    fn the_decoders_read_what_the_specification_writes() {
        assert_eq!(hex(b"48 65 6c6C6\n>junk"), b"Hell`");
        assert_eq!(base85(b"87cURD]i,\"Ebo80~>"), b"Hello World!");
        assert_eq!(base85(b"z!!~>"), [0, 0, 0, 0, 0]);
        assert_eq!(run_length(&[2, b'a', b'b', b'c', 254, b'x', 128, b'q'], MAX_DECODED), b"abcxxx");
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut e, &[7; 1000]).unwrap();
        assert_eq!(inflate(&e.finish().unwrap(), MAX_DECODED).unwrap(), [7; 1000]);
        // "-----A---B" in LZW, as the specification's example writes it (7.4.4.2).
        let lzw_example = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(lzw(&lzw_example, true, MAX_DECODED).unwrap(), b"-----A---B");
    }

    /// Zeros past the cap, compressed.
    fn zeros_past_the_cap() -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        for _ in 0..(MAX_DECODED >> 20) + 1 {
            std::io::Write::write_all(&mut e, &[0; 1 << 20]).unwrap();
        }
        e.finish().unwrap()
    }

    #[test]
    fn a_stream_that_decodes_past_the_cap_is_found_without_holding_it() {
        // Inflated in pieces: never more than one byte past the cap is held.
        let bomb = zeros_past_the_cap();
        assert!(bomb.len() < MAX_DECODED / 500);
        assert_eq!(inflate(&bomb, MAX_DECODED).map(|d| d.len()), Some(MAX_DECODED + 1));
        let d = dict("<< /Filter /FlateDecode >>");
        assert!(matches!(decode(&bomb, &d, false, false), Err(TooBig)));
        // Small enough that it can't get there: not decoded at all.
        assert!(matches!(decode(&[1, 2, 3], &d, false, false), Ok(None)));
        // An image may decode to its declared size, and no further.
        let image =
            |w: u32, h: u32| format!("<< /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode >>");
        assert!(decode(&bomb, &dict(&image(1 << 15, 1 << 14)), false, false).is_ok());
        assert!(matches!(decode(&bomb, &dict(&image(16, 16)), false, false), Err(TooBig)));
        assert_eq!(budget(&dict(&image(1 << 20, 1 << 20)), 0, false), MAX_IMAGE_DECODED);
    }

    #[test]
    fn only_streams_used_as_content_may_decode_past_the_cap() {
        let form = dict("<< /Type /XObject /Subtype /Form /Filter /FlateDecode >>");
        // A kilobyte asking for more than the cap is still refused.
        assert_eq!(budget(&form, 1 << 10, true), MAX_DECODED);
        // 100 MB of content may decode to 32 times that, up to 512 MB; 10 MB to 320 MB.
        assert_eq!(budget(&form, 100 << 20, true), MAX_CONTENT_DECODED);
        assert_eq!(budget(&form, 10 << 20, true), 320 << 20);
        // Anything else stays at the cap, however little its dictionary says: an Indexed colour
        // table (only Length and Filter), a mesh shading, a font.
        for other in ["<< /Filter /FlateDecode >>", "<< /ShadingType 4 /BitsPerCoordinate 16 /Filter /FlateDecode >>", "<< /Length1 5 /Filter /FlateDecode >>"] {
            assert_eq!(budget(&dict(other), 100 << 20, false), MAX_DECODED, "{other}");
            assert!(!content_by_kind(&dict(other)), "{other}");
        }
        assert!(content_by_kind(&form));
        assert!(content_by_kind(&dict("<< /Type /Pattern /PatternType 1 /PaintType 1 /Filter /FlateDecode >>")));
        // The zeros bomb (over 500 to one) is refused as content too.
        assert!(matches!(decode(&zeros_past_the_cap(), &form, true, true), Err(TooBig)));
        // The reader agrees: content up to 512 MB, every other stream (object and cross-reference
        // streams among them) 256 MB; the interpreter's page budget is the same 512 MB.
        let limits = craft_pdf::syntax::DecodeLimits::default();
        assert_eq!((limits.max_decoded_stream(), limits.max_decoded_content()), (MAX_DECODED, MAX_CONTENT_DECODED));
        assert_eq!(MAX_CONTENT_DECODED, 512 << 20);
    }

    /// `mib` MiB of comment lines of random digits ending in `tail`, Flate-compressed about 3.5 to
    /// one, as real drawings compress.
    fn ordinary_content(mib: usize, tail: &[u8]) -> Vec<u8> {
        let mut seed = 0x9e37_79b9_u32;
        let mut row = vec![0u8; 1 << 20];
        for b in &mut row {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *b = b'0' + (seed & 3) as u8;
        }
        row[0] = b'%';
        row[(1 << 20) - 1] = b'\n';
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        for _ in 0..mib {
            std::io::Write::write_all(&mut e, &row).unwrap();
        }
        std::io::Write::write_all(&mut e, tail).unwrap();
        let data = e.finish().unwrap();
        let ratio = ((mib << 20) + tail.len()) as f64 / data.len() as f64;
        assert!((2.0..=5.0).contains(&ratio), "{ratio}");
        data
    }

    /// A square, after 300 MiB of comments: past the cap, under the content limit (shared).
    fn content_300() -> &'static [u8] {
        static DATA: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        DATA.get_or_init(|| ordinary_content(300, b"\n0 0 10 10 re f\n"))
    }

    /// A one-page PDF drawing `data`: as its contents stream when `page` says so, or as the form
    /// `/F0` the page draws.
    fn page_pdf(data: &[u8], page: bool) -> Vec<u8> {
        let stream = |dict: &str, data: &[u8]| [format!("<< {dict} /Length {} >>\nstream\n", data.len()).as_bytes(), data, b"\nendstream"].concat();
        let shown: &[u8] = b"/F0 Do";
        let objects = [
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /XObject << /F0 5 0 R >> >> >>".to_vec(),
            if page { stream("/Filter /FlateDecode", data) } else { stream("", shown) },
            stream("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Filter /FlateDecode", if page { &[120, 156, 3, 0, 0, 0, 0, 1] } else { data }),
        ];
        write_pdf(&objects)
    }

    /// The objects, numbered from 1, as a PDF with a cross-reference table.
    fn write_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"%PDF-1.6\n".to_vec();
        let mut offsets = vec![];
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    fn drawn(pdf: &[u8]) -> Result<usize, crate::PdfError> {
        let doc = crate::import(pdf)?;
        let mut n = 0;
        doc.walk(|node| n += usize::from(!node.is_container()));
        Ok(n)
    }

    #[test]
    fn content_past_the_cap_at_an_ordinary_ratio_imports_whole() {
        let data = content_300();
        // As a form (DF394805's case) and as a page's own contents: the square after the
        // comments is drawn, so the content was read to its end.
        for page in [false, true] {
            assert_eq!(drawn(&page_pdf(data, page)).ok(), Some(1), "page contents: {page}");
        }
        // The reader decodes it whole only as content: as any other stream it stops at the cap.
        let pdf = crate::pages::open(&page_pdf(data, false), None).unwrap();
        let form = pdf.xref().get::<Stream<'_>>(craft_pdf::syntax::object::ObjectIdentifier::new(5, 0)).unwrap();
        assert_eq!(form.decoded().unwrap().len(), MAX_DECODED);
        assert_eq!(form.decoded_content_within(usize::MAX).unwrap().len(), (300 << 20) + 16);
    }

    #[test]
    fn other_streams_past_the_cap_are_refused_at_any_ratio() {
        let data = content_300();
        // The same data as an Indexed colour table the page uses, and as a mesh shading.
        let fill: &[u8] = b"/C0 cs 0 sc 0 0 10 10 re f";
        let pdf = write_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /ColorSpace << /C0 [/Indexed /DeviceRGB 255 5 0 R] >> >> >>".to_vec(),
            [format!("<< /Length {} >>\nstream\n", fill.len()).as_bytes(), fill, b"\nendstream"].concat(),
            [format!("<< /Filter /FlateDecode /Length {} >>\nstream\n", data.len()).as_bytes(), data, b"\nendstream"].concat(),
        ]);
        let err = crate::info(&pdf, None).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err.contains("would decode to more than 256 MB"), "{err}");
        let shading = dict("<< /ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 16 /BitsPerComponent 8 /BitsPerFlag 8 /Filter /FlateDecode >>");
        assert!(matches!(decode(data, &shading, true, false), Err(TooBig)));
        // An object stream or a cross-reference stream: read before the file opens.
        let stream = |dict: &str| [format!("1 0 obj\n<< {dict} /Length {} >>\nstream\n", data.len()).as_bytes(), data, b"\nendstream\nendobj\n"].concat();
        assert!(unsafe_file_streams(&stream("/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode")));
        assert!(unsafe_file_streams(&stream("/Type /XRef /W [1 2 1] /Filter /FlateDecode")));
    }

    #[test]
    fn content_past_the_content_limit_is_refused() {
        let data = ordinary_content((MAX_CONTENT_DECODED >> 20) + 16, b"\n0 0 10 10 re f\n");
        for page in [false, true] {
            let err = crate::info(&page_pdf(&data, page), None).err().map(|e| e.to_string()).unwrap_or_default();
            assert!(err.contains("512 MB"), "page contents {page}: {err}");
        }
    }

    /// A one-page PDF drawing one flat 8-bit DeviceRGB image of `w` × `h` pixels, Flate-compressed.
    fn one_image_pdf(w: usize, h: usize) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        let row: Vec<u8> = [200, 180, 150].repeat(w);
        for _ in 0..h {
            std::io::Write::write_all(&mut e, &row).unwrap();
        }
        let image = e.finish().unwrap();
        let content = b"q 595 0 0 842 0 0 cm /Im0 Do Q";
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R /Resources << /XObject << /Im0 5 0 R >> >> >>".to_vec(),
            [format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(), content, b"\nendstream"].concat(),
            [
                format!(
                    "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
                    image.len()
                )
                .as_bytes(),
                &image,
                b"\nendstream",
            ]
            .concat(),
        ];
        write_pdf(&objects)
    }

    #[test]
    fn an_image_past_the_stream_cap_within_its_declared_size_opens() {
        // An A0 sheet scanned at 300 ppi: 9930 × 14040 RGB pixels, 418 MB decoded (#2005).
        let pdf = one_image_pdf(9930, 14040);
        let info = crate::info(&pdf, None);
        assert!(info.is_ok(), "{:?}", info.err());
        assert_eq!(info.map(|i| i.pages.len()).unwrap(), 1);
    }

    #[test]
    fn object_and_cross_reference_streams_are_looked_at_before_the_file_is_read() {
        let stream = |dict: &str, data: &[u8]| {
            [format!("1 0 obj\n<< {dict} /Length {} >>\nstream\n", data.len()).as_bytes(), data, b"\nendstream\nendobj\n"].concat()
        };
        for bad in [
            stream("/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /DecodeParms << /Predictor 2 /Columns 0 >>", b"x"),
            stream("/Type /XRef /Filter /FlateDecode /DecodeParms 9 0 R", b"x"),
            stream("/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode", &zeros_past_the_cap()),
        ] {
            assert!(unsafe_file_streams(&bad));
        }
        for fine in [
            stream("/Type /XRef /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 5 >>", b"x"),
            // Another kind of stream is looked at once the file is open, its references followed.
            stream("/Filter /FlateDecode /DecodeParms << /Predictor 2 /Columns 0 >>", b"x"),
        ] {
            assert!(!unsafe_file_streams(&fine));
        }
    }
}
