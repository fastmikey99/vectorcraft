//! Raw image-use accounting. Metadata and placement are evidence, not decoded ink coverage.
//! ISO 32000-1 §§8.9–8.10; inline aliases must not silently choose between conflicting keys.
use craft_pdf::syntax::object::{Array, Dict, Name, Object, Stream};
use craft_pdf::syntax::page::Resources;
use serde::Serialize;

pub(crate) const MAX_IMAGES: usize = 4096;
#[derive(Clone, Debug, Serialize)]
pub struct PrintImageUse {
    pub layer: Option<usize>,
    /// Number of preceding path-paint events on this page; image order is the list order.
    pub before_path: usize,
    pub inline: bool,
    /// Unit image square to default page user space, before page Rotate/UserUnit.
    /// This is placement, never a painted-coverage boundary.
    pub matrix: Option<[f64; 6]>,
    pub clip_context: bool,
    pub overprint: bool,
    pub overprint_mode: u8,
    pub description: PrintImageDescription,
}
#[derive(Clone, Debug, Serialize)]
pub struct PrintImageDescription {
    pub dimensions: Option<[u32; 2]>,
    pub bits_per_component: Option<u8>,
    pub stencil: bool,
    pub interpolate: bool,
    pub colour_space: String,
    /// Exact PDF name bytes, hex-encoded; no lossy text-name normalization.
    pub colourants_hex: Vec<String>,
    pub decode: Option<Vec<f64>>,
    pub mask: String,
    /// Presence of an SMask entry; its samples and validity remain unreconciled.
    pub soft_mask: bool,
    pub embedded_alpha: bool,
    /// Presence of the image's own OC entry, independent of enclosing marked content.
    pub optional_content: bool,
    pub external_data: bool,
    pub issues: Vec<String>,
}
fn key<'a>(d: &Dict<'_>, long: &'a [u8], short: &'a [u8], inline: bool, issues: &mut Vec<String>) -> &'a [u8] {
    if inline && d.contains_key(long) && d.contains_key(short) {
        issues.push(format!("Ambiguous inline image aliases {} and {}", String::from_utf8_lossy(long), String::from_utf8_lossy(short)));
    }
    if !inline || d.contains_key(long) { long } else { short }
}
fn flag(d: &Dict<'_>, key: &[u8], issues: &mut Vec<String>) -> bool {
    if d.contains_key(key) && d.get::<bool>(key).is_none() {
        issues.push(format!("Invalid image boolean {}", String::from_utf8_lossy(key)));
    }
    d.get::<bool>(key).unwrap_or(false)
}
fn space(o: Option<Object<'_>>, res: &Resources<'_>, depth: u8, out: &mut PrintImageDescription) {
    if depth > 4 {
        out.issues.push("Image colour-space resource recursion exceeds budget".into());
        return;
    }
    let Some(o) = o else {
        out.issues.push("Image colour-space resource is missing or unresolved".into());
        return;
    };
    let (kind, names) = match o {
        Object::Name(n) => {
            let direct = match n.as_ref() {
                b"DeviceGray" | b"G" => Some(("DeviceGray", b"DefaultGray".as_slice())),
                b"DeviceRGB" | b"RGB" => Some(("DeviceRGB", b"DefaultRGB".as_slice())),
                b"DeviceCMYK" | b"CMYK" => Some(("DeviceCMYK", b"DefaultCMYK".as_slice())),
                _ => None,
            };
            if let Some((kind, default)) = direct {
                out.colour_space = kind.into();
                if Name::new(default).and_then(|n| res.get_color_space(&n)).is_some() {
                    out.issues.push("Image device colour space has a default substitution".into());
                }
            } else {
                space(res.get_color_space(&n), res, depth + 1, out);
            }
            return;
        }
        Object::Array(a) => {
            let mut it = a.flex_iter();
            let kind = it.next::<Name<'_>>();
            match kind.as_deref() {
                Some(b"Separation") => ("Separation", it.next::<Name<'_>>().into_iter().collect::<Vec<_>>()),
                Some(b"DeviceN") => {
                    let Some(names) = it.next::<Array<'_>>() else {
                        out.issues.push("Missing image DeviceN colourant array".into());
                        return;
                    };
                    // Bound before copying names, including malformed non-name entries.
                    let objects: Vec<_> = names.iter::<Object<'_>>().take(33).collect();
                    if objects.is_empty() || objects.len() > 32 || objects.iter().any(|v| !matches!(v, Object::Name(_))) {
                        out.issues.push("Invalid or oversized image DeviceN colourant array".into());
                        return;
                    }
                    ("DeviceN", objects.into_iter().filter_map(|v| if let Object::Name(n) = v { Some(n) } else { None }).collect())
                }
                Some(b"Indexed" | b"I") => ("Indexed", vec![]),
                Some(b"ICCBased") => ("ICCBased", vec![]),
                Some(b"CalGray") => ("CalGray", vec![]),
                Some(b"CalRGB") => ("CalRGB", vec![]),
                Some(b"Lab") => ("Lab", vec![]),
                _ => {
                    out.issues.push("Unrecognised image colour-space array".into());
                    return;
                }
            }
        }
        _ => {
            out.issues.push("Invalid image colour-space object".into());
            return;
        }
    };
    out.colour_space = kind.into();
    if kind == "Separation" && names.is_empty() {
        out.issues.push("Missing image Separation colourant name".into());
    }
    if names.iter().map(|n| n.as_ref().len()).sum::<usize>() > 1024 {
        out.issues.push("Image colourant-name byte budget exceeded".into());
        return;
    }
    for n in names {
        let hex: String = n.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        out.colourants_hex.push(hex);
    }
}
pub(crate) fn describe(stream: &Stream<'_>, res: &Resources<'_>, inline: bool) -> PrintImageDescription {
    let d = stream.dict();
    let mut out = PrintImageDescription {
        dimensions: None,
        bits_per_component: None,
        stencil: false,
        interpolate: false,
        colour_space: "unresolved".into(),
        colourants_hex: vec![],
        decode: None,
        mask: "none".into(),
        soft_mask: d.contains_key(b"SMask"),
        embedded_alpha: d.get::<u8>(b"SMaskInData").is_some_and(|n| n == 1 || n == 2),
        optional_content: d.contains_key(b"OC"),
        external_data: d.contains_key(b"F") && !inline,
        issues: vec![],
    };
    if d.contains_key(b"SMaskInData") && d.get::<u8>(b"SMaskInData").is_none_or(|n| n > 2) {
        out.issues.push("Invalid image SMaskInData value".into());
    }
    let w = key(d, b"Width", b"W", inline, &mut out.issues);
    let h = key(d, b"Height", b"H", inline, &mut out.issues);
    out.dimensions = d.get::<u32>(w).zip(d.get::<u32>(h)).filter(|(w, h)| *w > 0 && *h > 0).map(|(w, h)| [w, h]);
    if out.dimensions.is_none() {
        out.issues.push("Invalid or missing image dimensions".into());
    }
    let im = key(d, b"ImageMask", b"IM", inline, &mut out.issues);
    out.stencil = flag(d, im, &mut out.issues);
    let interpolate = key(d, b"Interpolate", b"I", inline, &mut out.issues);
    out.interpolate = flag(d, interpolate, &mut out.issues);
    let bpc = key(d, b"BitsPerComponent", b"BPC", inline, &mut out.issues);
    out.bits_per_component = d.get::<u8>(bpc).or_else(|| (out.stencil && !d.contains_key(bpc)).then_some(1));
    if out.bits_per_component.is_none_or(|n| ![1, 2, 4, 8, 16].contains(&n) || (out.stencil && n != 1)) {
        out.issues.push("Image sample depth requires explicit decoding".into());
    }
    let cs = key(d, b"ColorSpace", b"CS", inline, &mut out.issues);
    if out.stencil {
        out.colour_space = "stencil_current_fill".into();
    } else {
        space(d.get::<Object<'_>>(cs), res, 0, &mut out);
    }
    let decode = key(d, b"Decode", b"D", inline, &mut out.issues);
    if d.contains_key(decode) {
        if let Some(a) = d.get::<Array<'_>>(decode) {
            let values: Vec<_> = a.iter::<Object<'_>>().take(65).collect();
            if values.is_empty()
                || values.len() > 64
                || values.len() % 2 != 0
                || values.iter().any(|v| !matches!(v, Object::Number(n) if n.as_f64().is_finite()))
            {
                out.issues.push("Invalid or oversized image Decode array".into());
            } else {
                out.decode = Some(values.iter().filter_map(|v| if let Object::Number(n) = v { Some(n.as_f64()) } else { None }).collect());
            }
        } else {
            out.issues.push("Unresolved image Decode array".into());
        }
    }
    if d.contains_key(b"Mask") {
        out.mask = match d.get::<Object<'_>>(b"Mask") {
            Some(Object::Array(_)) => "colour_key",
            Some(Object::Stream(_)) => "image",
            _ => "unresolved",
        }
        .into();
    }
    // These pairs also have ambiguous inline semantics even though this census does not decode samples.
    key(d, b"Filter", b"F", inline, &mut out.issues);
    key(d, b"DecodeParms", b"DP", inline, &mut out.issues);
    out
}
