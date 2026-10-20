//! CMYK images with spot channels ([`vectorcraft_doc::cmyk::spots`]) as DeviceN images: the
//! process inks and each spot ink a colorant of its own, named as its swatch, so that each prints
//! on its plate.
//!
//! The PDF writer has no DeviceN images. The export draws a small CMYK stand-in image for each
//! ([`placeholder`]), its samples a marker no other image has, and once the file is written
//! [`write`] finds each stand-in by its marker and writes the real image in its place: its size,
//! a `[/DeviceN [/Cyan /Magenta /Yellow /Black spots…] alt tint]` colour space (the alternate
//! space the writer gave the stand-in, the tint transform printing each spot ink's process
//! equivalent over the process inks) and its samples, and its alpha in the stand-in's soft mask.

use std::io::{Read, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use craft_pdf::write::backend as krilla;
use krilla::image::{BitsPerComponent, CustomImage, Image, ImageColorspace};
use vectorcraft_doc::ImageBlob;

use crate::PdfError;
use crate::lab_spot::{find, rfind};
use crate::patch::{Patch, Xref, number};

/// Pixels of a stand-in image (one row): 32 bytes of CMYK samples.
const MARK_PIXELS: u32 = 8;
/// What a stand-in's samples start with; a counter unique in the process follows.
const MARK: &[u8; 20] = b"VectorCraft DeviceN:";

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A stand-in image: its marker samples, and an opaque alpha channel when the image has one.
#[derive(Clone, Hash)]
struct StandIn {
    mark: [u8; 32],
    alpha: Option<[u8; MARK_PIXELS as usize]>,
}

impl CustomImage for StandIn {
    fn color_channel(&self) -> &[u8] {
        &self.mark
    }

    fn alpha_channel(&self) -> Option<&[u8]> {
        self.alpha.as_ref().map(|a| a.as_slice())
    }

    fn bits_per_component(&self) -> BitsPerComponent {
        BitsPerComponent::Eight
    }

    fn size(&self) -> (u32, u32) {
        (MARK_PIXELS, 1)
    }

    fn icc_profile(&self) -> Option<&[u8]> {
        None
    }

    fn color_space(&self) -> ImageColorspace {
        ImageColorspace::Cmyk
    }
}

/// An image with spot channels, drawn as a stand-in until the file is written.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    mark: [u8; 32],
    width: u32,
    height: u32,
    /// The spot inks' names, after the four process inks.
    spots: Vec<String>,
    /// Each spot ink's process equivalent (C, M, Y, K 0..1).
    alts: Vec<[f32; 4]>,
    /// The samples: C, M, Y, K, then the spot inks, a byte each, pixel by pixel.
    samples: Arc<Vec<u8>>,
    alpha: Option<Arc<Vec<u8>>>,
    interpolate: bool,
}

/// The stand-in drawn for CMYK image `blob` with spot channels, and what [`write`] puts in its
/// place: `alt` gives each spot ink's process equivalent, `alpha` (a byte a pixel) is the image's
/// soft mask. `None` when `blob` has no spot channels or they can't be read.
pub(crate) fn placeholder(blob: &ImageBlob, alt: impl Fn(&str) -> [f32; 4], alpha: Option<Vec<u8>>, interpolate: bool) -> Option<(Image, Pending)> {
    let inks = blob.cmyk()?;
    let spots = blob.spot_inks()?;
    if (inks.width, inks.height) != (spots.width, spots.height) {
        return None;
    }
    let n = usize::try_from(u64::from(inks.width) * u64::from(inks.height)).ok()?;
    if alpha.as_ref().is_some_and(|a| a.len() != n) {
        return None;
    }
    let per = spots.names.len();
    let samples: Vec<u8> =
        inks.data.as_chunks::<4>().0.iter().zip(spots.data.chunks_exact(per.max(1))).flat_map(|(p, s)| p.iter().chain(s).copied()).collect();
    let mut mark = [0u8; 32];
    mark[..MARK.len()].copy_from_slice(MARK);
    mark[MARK.len()..MARK.len() + 8].copy_from_slice(&NEXT.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    let stand_in = StandIn { mark, alpha: alpha.as_ref().map(|_| [u8::MAX; MARK_PIXELS as usize]) };
    let image = Image::from_custom(stand_in, interpolate).ok()?;
    let alts = spots.names.iter().map(|n| alt(n)).collect();
    let pending = Pending {
        mark,
        width: inks.width,
        height: inks.height,
        spots: spots.names,
        alts,
        samples: Arc::new(samples),
        alpha: alpha.map(Arc::new),
        interpolate,
    };
    Some((image, pending))
}

/// `name` as a PDF name object.
fn pdf_name(name: &str) -> String {
    let mut out = String::from("/");
    for &b in name.as_bytes() {
        let regular = !b"\0\t\n\x0C\r ()<>[]{}/%#".contains(&b);
        if (b'!'..=b'~').contains(&b) && regular {
            out.push(b as char);
        } else {
            out.push_str(&format!("#{b:02X}"));
        }
    }
    out
}

/// A number for PDF syntax: at most 4 decimals, no trailing zeros.
fn num(v: f32) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.into() }
}

/// The PostScript calculator program of the tint transform of process inks and spot inks `alts`
/// (their process equivalents): each spot ink prints its share of each process ink over the
/// process inks, `1 - (1 - process) × ∏(1 - tint × share)`, as the renderer mixes them.
fn tint_program(alts: &[[f32; 4]]) -> String {
    let inputs = 4 + alts.len();
    let mut p = String::from("{");
    for j in 0..4 {
        // `j` outputs are on the stack above the inputs, so input `i` is `inputs - 1 - i + j`
        // from the top: process ink `j` is `inputs - 1`; once the paper left by it is pushed,
        // spot ink `i` is `inputs - 1 - (4 + i) + j + 1`.
        p.push_str(&format!(" {} index 1 exch sub", inputs - 1));
        for (i, alt) in alts.iter().enumerate() {
            let a = alt.get(j).copied().filter(|a| a.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0);
            if a > 0.0 {
                p.push_str(&format!(" {} index {} mul 1 exch sub mul", inputs - 1 - (4 + i) + j + 1, num(a)));
            }
        }
        p.push_str(" 1 exch sub");
    }
    // The four outputs below the inputs, then the inputs dropped.
    p.push_str(&format!(" {} 4 roll", inputs + 4));
    for _ in 0..inputs {
        p.push_str(" pop");
    }
    p.push_str(" }");
    p
}

fn deflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).ok()?;
    e.finish().ok()
}

/// The stream data of the object whose dictionary `dict` gives ([`Xref::dict`]: just after its
/// `<<`, up to its `endobj`), decoded when it has no filter or Flate alone; `None` otherwise.
fn stream_data(pdf: &[u8], (start, end): (usize, usize), most: usize) -> Option<Vec<u8>> {
    let body = pdf.get(start..end)?;
    let s = find(body, b"stream", 0)?;
    let mut at = s + 6;
    if body.get(at) == Some(&b'\r') {
        at += 1;
    }
    if body.get(at) == Some(&b'\n') {
        at += 1;
    }
    let e = rfind(body, b"endstream")?;
    let raw = body.get(at..e)?;
    let head = body.get(..s)?;
    if find(head, b"/Filter", 0).is_none() {
        let raw = raw.strip_suffix(b"\n").map_or(raw, |r| r.strip_suffix(b"\r").unwrap_or(r));
        return raw.get(..most.min(raw.len())).map(<[u8]>::to_vec);
    }
    if find(head, b"/FlateDecode", 0).is_none() || find(head, b"/DecodeParms", 0).is_some() {
        return None;
    }
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(raw).take(most as u64).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Where the value of `key` starts in dictionary text `dict`.
fn value_at(dict: &[u8], key: &[u8]) -> Option<usize> {
    let mut from = 0;
    loop {
        let at = find(dict, key, from)? + key.len();
        // A key that only starts with `key` (`/SMaskInData` for `/SMask`) isn't it.
        if !dict.get(at).is_some_and(|b| b.is_ascii_alphanumeric()) {
            let mut at = at;
            while dict.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            return Some(at);
        }
        from = at;
    }
}

/// The unsigned number `key` has in dictionary text `dict`.
fn number_of(dict: &[u8], key: &[u8]) -> Option<usize> {
    number(dict, value_at(dict, key)?).map(|(v, _)| v)
}

/// The value of `key` in dictionary text `dict`: a name, or an object reference (`n 0 R`).
fn value<'a>(dict: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let start = value_at(dict, key)?;
    let mut at = start;
    if dict.get(at) == Some(&b'/') {
        at += 1;
        while dict.get(at).is_some_and(|b| !b.is_ascii_whitespace() && !b"/<>[]()".contains(b)) {
            at += 1;
        }
        return dict.get(start..at);
    }
    let (_, after) = number(dict, at)?;
    let (_, mut after) = number(dict, after)?;
    while dict.get(after).is_some_and(u8::is_ascii_whitespace) {
        after += 1;
    }
    (dict.get(after) == Some(&b'R')).then(|| dict.get(start..after + 1)).flatten()
}

/// An image XObject's dictionary and stream: `<<…>>stream…endstream`.
fn image_object(width: u32, height: u32, space: &[u8], data: &[u8], interpolate: bool, smask: Option<&[u8]>) -> Vec<u8> {
    let mut o = format!("<</Type/XObject/Subtype/Image/Width {width}/Height {height}/ColorSpace ").into_bytes();
    o.extend_from_slice(space);
    o.extend_from_slice(b"/BitsPerComponent 8");
    if interpolate {
        o.extend_from_slice(b"/Interpolate true");
    }
    if let Some(s) = smask {
        o.extend_from_slice(b"/SMask ");
        o.extend_from_slice(s);
    }
    o.extend_from_slice(format!("/Filter/FlateDecode/Length {}>>\nstream\n", data.len()).as_bytes());
    o.extend_from_slice(data);
    o.extend_from_slice(b"\nendstream\n");
    o
}

/// The file `pdf` (as the PDF writer wrote it) with the images `pending` drew stand-ins for
/// written in their place ([`placeholder`]). An error when a stand-in can't be replaced, so that
/// no stand-in is ever left in a file.
pub(crate) fn write(pdf: Vec<u8>, pending: &[Pending]) -> Result<Vec<u8>, PdfError> {
    if pending.is_empty() {
        return Ok(pdf);
    }
    let fail = || PdfError::Write("images with spot channels couldn't be written as DeviceN images".into());
    let xref = Xref::read(&pdf).ok_or_else(fail)?;
    let mut patch = Patch::new(&xref);
    let mut spaces: Vec<(String, Vec<u8>)> = vec![];
    let mut done = vec![false; pending.len()];
    for (n, _) in xref.objects(&pdf) {
        let Some(range) = xref.dict(&pdf, n) else { continue };
        let Some(head) = pdf.get(range.0..range.1).and_then(|b| b.get(..find(b, b"stream", 0)?)) else { continue };
        if find(head, b"/Image", 0).is_none() || number_of(head, b"/Width") != Some(MARK_PIXELS as usize) || number_of(head, b"/Height") != Some(1) {
            continue;
        }
        let Some(data) = stream_data(&pdf, range, 64) else { continue };
        let Some(i) = pending.iter().position(|p| data == p.mark) else { continue };
        let (Some(p), Some(seen)) = (pending.get(i), done.get_mut(i)) else { continue };
        if *seen {
            continue;
        }
        *seen = true;
        // The stand-in's own colour space (DeviceCMYK, or the CMYK profile's) is the alternate.
        let alt = value(head, b"/ColorSpace").ok_or_else(fail)?.to_vec();
        let key = format!("{}\0{:?}\0{}", p.spots.join("\0"), p.alts, String::from_utf8_lossy(&alt));
        let space = match spaces.iter().find(|(k, _)| *k == key) {
            Some((_, s)) => s.clone(),
            None => {
                let program = tint_program(&p.alts);
                let inputs = 4 + p.spots.len();
                let domain = vec!["0 1"; inputs].join(" ");
                let mut f = format!("<</FunctionType 4/Domain[{domain}]/Range[0 1 0 1 0 1 0 1]/Length {}>>\nstream\n", program.len()).into_bytes();
                f.extend_from_slice(program.as_bytes());
                f.extend_from_slice(b"\nendstream");
                let func = patch.add_object(f);
                let names: String = ["Cyan", "Magenta", "Yellow", "Black"]
                    .iter()
                    .map(|s| s.to_string())
                    .chain(p.spots.iter().cloned())
                    .map(|s| pdf_name(&s))
                    .collect();
                let mut arr = format!("[/DeviceN[{names}]").into_bytes();
                arr.extend_from_slice(&alt);
                arr.extend_from_slice(format!(" {func} 0 R]").as_bytes());
                let s = format!("{} 0 R", patch.add_object(arr)).into_bytes();
                spaces.push((key, s.clone()));
                s
            }
        };
        let smask = value(head, b"/SMask").map(<[u8]>::to_vec);
        match (&p.alpha, &smask) {
            (Some(alpha), Some(m)) => {
                let m = u32::try_from(number(m, 0).ok_or_else(fail)?.0).map_err(|_| fail())?;
                let mrange = xref.dict(&pdf, m).ok_or_else(fail)?;
                let data = deflate(alpha).ok_or_else(fail)?;
                let end = rfind(pdf.get(..mrange.1).ok_or_else(fail)?, b"endstream").ok_or_else(fail)? + b"endstream".len();
                patch.replace(mrange.0 - 2, end, image_object(p.width, p.height, b"/DeviceGray", &data, p.interpolate, None));
            }
            (None, None) => {}
            _ => return Err(fail()),
        }
        let data = deflate(&p.samples).ok_or_else(fail)?;
        let end = rfind(pdf.get(..range.1).ok_or_else(fail)?, b"endstream").ok_or_else(fail)? + b"endstream".len();
        patch.replace(range.0 - 2, end, image_object(p.width, p.height, &space, &data, p.interpolate, smask.as_deref()));
    }
    // A stand-in the file doesn't draw (its page left out) needs nothing; one that is drawn was
    // found above.
    if !done.iter().any(|d| *d) {
        return Ok(pdf);
    }
    patch.apply(&pdf, &xref)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_programs_are_pdf_syntax() {
        assert_eq!(pdf_name("PANTONE 7479 C"), "/PANTONE#207479#20C");
        assert_eq!(pdf_name("Encre é#"), "/Encre#20#C3#A9#23");
        // One spot ink at C 50 % and Y 100 %: inputs c m y k s.
        assert_eq!(
            tint_program(&[[0.5, 0.0, 1.0, 0.0]]),
            "{ 4 index 1 exch sub 1 index 0.5 mul 1 exch sub mul 1 exch sub 4 index 1 exch sub 1 exch sub 4 index 1 exch sub 3 index 1 mul 1 exch sub mul 1 exch sub 4 index 1 exch sub 1 exch sub 9 4 roll pop pop pop pop pop }"
        );
    }

    /// The program runs in the PDF reader's own calculator and mixes the inks as the renderer does.
    #[test]
    fn the_tint_transform_prints_the_spot_inks_over_the_process_inks() {
        let program = tint_program(&[[0.5, 0.0, 1.0, 0.0], [0.0, 1.0, 0.0, 0.0]]);
        let mut obj = format!("<< /FunctionType 4 /Domain [0 1 0 1 0 1 0 1 0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length {} >>\nstream\n", program.len())
            .into_bytes();
        obj.extend_from_slice(program.as_bytes());
        obj.extend_from_slice(b"\nendstream");
        let objs = [b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec(), obj];
        let mut file = b"%PDF-1.4\n".to_vec();
        let mut at = vec![];
        for (i, o) in objs.iter().enumerate() {
            at.push(file.len());
            file.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            file.extend_from_slice(o);
            file.extend_from_slice(b"\nendobj\n");
        }
        let xref = file.len();
        file.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        for a in at {
            file.extend_from_slice(format!("{a:010} 00000 n \n").as_bytes());
        }
        file.extend_from_slice(format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
        let pdf = craft_pdf::syntax::Pdf::new(Arc::new(file)).unwrap();
        let o = pdf.objects().into_iter().find(|o| matches!(o, craft_pdf::syntax::object::Object::Stream(_))).unwrap();
        let f = craft_pdf::interpret::Function::new(&o).unwrap();
        let out = f.eval([0.25, 0.0, 0.0, 0.5, 1.0, 0.5].into_iter().collect()).unwrap();
        let want = [1.0 - 0.75 * 0.5, 0.5, 1.0, 0.5];
        assert!(out.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-5), "{out:?}");
    }
}
