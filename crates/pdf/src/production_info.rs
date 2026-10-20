//! Bounded raw production metadata, independent of editor state and embedded native data.
//! ISO 32000-1 §§7.7.3, 14.11.2 and 14.11.5; PDF 2.0 page OutputIntents are
//! retained separately, never silently replaced with the catalog's intent.
use crate::PdfError;
use craft_pdf::syntax::object::{Dict, Name, Object, Stream};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize)]
pub struct ProductionPage {
    /// Unclipped boxes in PDF user space, scaled to physical millimetres, y up.
    pub media_mm: [f64; 4],
    pub crop_mm: [f64; 4],
    pub bleed_mm: [f64; 4],
    pub trim_mm: [f64; 4],
    pub art_mm: [f64; 4],
    pub rotation: u16,
    pub user_unit: f64,
    pub intents: Option<Vec<ProductionIntent>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProductionIntent {
    pub subtype: String,
    pub profile_sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProductionInfo {
    pub pages: Vec<ProductionPage>,
    pub intents: Option<Vec<ProductionIntent>>,
}
fn invalid(s: &str) -> PdfError {
    PdfError::Unsupported(format!("Production metadata: {s}"))
}
fn inherited<'a>(dict: &Dict<'a>, key: &[u8]) -> Result<Option<Object<'a>>, PdfError> {
    let mut dict = dict.clone();
    for _ in 0..32 {
        if let Some(value) = dict.get::<Object<'a>>(key) {
            return Ok(Some(value));
        }
        if dict.get_raw::<Object<'a>>(key).is_some() {
            return Err(invalid("unreadable inherited value"));
        }
        match dict.get::<Dict<'a>>(b"Parent") {
            Some(parent) => dict = parent,
            None if dict.contains_key(b"Parent") => return Err(invalid("unreadable page Parent")),
            None => return Ok(None),
        }
    }
    Err(invalid("page inheritance exceeds budget"))
}
fn page_box(dict: &Dict<'_>, key: &[u8], inherit: bool, fallback: Option<[f64; 4]>) -> Result<[f64; 4], PdfError> {
    let object = if inherit { inherited(dict, key)? } else { dict.get::<Object<'_>>(key) };
    let Some(object) = object else {
        if dict.get_raw::<Object<'_>>(key).is_some() {
            return Err(invalid("unreadable page box"));
        }
        return fallback.ok_or_else(|| invalid("missing media box"));
    };
    let Object::Array(array) = object else {
        return Err(invalid("page box is not an array"));
    };
    let values: Vec<f64> = array.iter::<f64>().collect();
    let [x0, y0, x1, y1] = values.as_slice() else {
        return Err(invalid("page box needs four numbers"));
    };
    if !crate::import_color::readable(&array)
        || crate::import_color::items(&array).count() != 4
        || values.iter().any(|n| !n.is_finite() || n.abs() > 1.0e9)
        || x0 >= x1
        || y0 >= y1
    {
        return Err(invalid("invalid or reversed page box"));
    }
    Ok([*x0, *y0, *x1, *y1])
}
fn intents(dict: &Dict<'_>, budget: &mut usize) -> Result<Option<Vec<ProductionIntent>>, PdfError> {
    let Some(value) = dict.get::<Object<'_>>(b"OutputIntents") else {
        if dict.get_raw::<Object<'_>>(b"OutputIntents").is_some() {
            return Err(invalid("unreadable OutputIntents"));
        }
        return Ok(None);
    };
    let Object::Array(array) = value else {
        return Err(invalid("OutputIntents is not an array"));
    };
    if !crate::import_color::readable(&array) {
        return Err(invalid("unreadable output intent array"));
    }
    let count = crate::import_color::items(&array).count();
    *budget = budget.checked_sub(count).ok_or_else(|| invalid("output intent budget exceeded"))?;
    let mut result = vec![];
    for intent in array.iter::<Dict<'_>>() {
        if intent.get_raw::<Object<'_>>(b"DestOutputProfileRef").is_some() {
            return Err(invalid("external output profile requires retained external evidence"));
        }
        let subtype = intent.get::<Name<'_>>(b"S").ok_or_else(|| invalid("output intent subtype missing"))?;
        let subtype = std::str::from_utf8(subtype.as_ref()).map_err(|_| invalid("non-UTF8 intent subtype"))?.to_owned();
        let profile_sha256 = if let Some(profile) = intent.get::<Stream<'_>>(b"DestOutputProfile") {
            let bytes = profile.decoded_within(8 * 1024 * 1024 + 1).map_err(|_| invalid("output profile decode budget or syntax"))?;
            if bytes.len() > 8 * 1024 * 1024 {
                return Err(invalid("output profile exceeds decoded byte budget"));
            }
            if bytes.len() < 128 || bytes.get(36..40) != Some(b"acsp") {
                return Err(invalid("output profile has no ICC header"));
            }
            Some(format!("{:x}", Sha256::digest(&bytes)))
        } else {
            if intent.get_raw::<Object<'_>>(b"DestOutputProfile").is_some() {
                return Err(invalid("unreadable output profile"));
            }
            None
        };
        result.push(ProductionIntent { subtype, profile_sha256 });
    }
    if result.len() != count {
        return Err(invalid("unreadable output intent entry"));
    }
    Ok(Some(result))
}
pub fn production_info(bytes: &[u8]) -> Result<ProductionInfo, PdfError> {
    let pdf = crate::pages::open_with_options(bytes, None, &craft_pdf::OpenOptions { max_input_bytes: 256 * 1024 * 1024, ..Default::default() })?;
    if pdf.pages().len() > 1000 {
        return Err(invalid("page budget exceeded"));
    }
    let xref = pdf.xref();
    let catalog = xref.get::<Dict<'_>>(xref.root_id()).ok_or_else(|| invalid("catalog missing"))?;
    let mut intent_budget = 64;
    let catalog_intents = intents(&catalog, &mut intent_budget)?;
    let mut pages = vec![];
    for page in pdf.pages().iter() {
        let raw = page.raw();
        let unit = if raw.get_raw::<Object<'_>>(b"UserUnit").is_some() {
            raw.get::<f64>(b"UserUnit").filter(|n| n.is_finite() && *n > 0.0 && *n <= 75_000.0).ok_or_else(|| invalid("invalid UserUnit"))?
        } else {
            1.0
        };
        let rotation = match inherited(raw, b"Rotate")? {
            None => 0,
            Some(Object::Number(n)) if n.as_f64().is_finite() && n.as_f64() % 90.0 == 0.0 => n.as_f64().rem_euclid(360.0) as u16,
            _ => return Err(invalid("invalid page rotation")),
        };
        let media = page_box(raw, b"MediaBox", true, None)?;
        let crop = page_box(raw, b"CropBox", true, Some(media))?;
        let scale = |b: [f64; 4]| b.map(|n| n * unit * 25.4 / 72.0);
        pages.push(ProductionPage {
            media_mm: scale(media),
            crop_mm: scale(crop),
            bleed_mm: scale(page_box(raw, b"BleedBox", false, Some(crop))?),
            trim_mm: scale(page_box(raw, b"TrimBox", false, Some(crop))?),
            art_mm: scale(page_box(raw, b"ArtBox", false, Some(crop))?),
            rotation,
            user_unit: unit,
            intents: intents(raw, &mut intent_budget)?,
        });
    }
    Ok(ProductionInfo { pages, intents: catalog_intents })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_testkit::pdf::{PdfPage, pdf, pdf_with_catalog};
    #[test]
    fn keeps_unclipped_boxes_physical_units_rotation_and_page_intents() {
        let mut page = PdfPage::new(72.0, 144.0, "");
        page.crop = Some([-1.0, -2.0, 73.0, 145.0]);
        page.rotate = -90;
        page.entries = "/UserUnit 2 /OutputIntents [<< /S /PageIntent >>]".into();
        let bytes = pdf_with_catalog(&[page], &[], "/OutputIntents [<< /S /CatalogIntent >>]", None);
        let info = production_info(&bytes).unwrap();
        assert_eq!(info.pages[0].rotation, 270);
        assert_eq!(info.pages[0].media_mm, [0.0, 0.0, 50.8, 101.6]);
        assert_eq!(info.pages[0].crop_mm[0], -2.0 * 25.4 / 72.0);
        assert_eq!(info.pages[0].crop_mm, info.pages[0].trim_mm);
        assert_eq!(info.intents.as_ref().unwrap()[0].subtype, "CatalogIntent");
        assert_eq!(info.pages[0].intents.as_ref().unwrap()[0].subtype, "PageIntent");
    }
    #[test]
    fn rejects_unreadable_or_unsupported_metadata_instead_of_defaulting() {
        for entries in [
            "/Parent 99 0 R",
            "/UserUnit -1",
            "/UserUnit (2)",
            "/Rotate 89",
            "/TrimBox [0 0 5]",
            "/ArtBox [4 0 1 3]",
            "/OutputIntents [123]",
            "/OutputIntents [99 0 R]",
            "/OutputIntents 99 0 R",
            "/OutputIntents [<< /S /Test /DestOutputProfile 99 0 R >>]",
            "/OutputIntents [<< /S /Test /DestOutputProfileRef << >> >>]",
        ] {
            let mut page = PdfPage::new(72.0, 72.0, "");
            page.entries = entries.into();
            assert!(production_info(&pdf(&[page], None)).is_err(), "{entries}");
        }
    }
    #[test]
    fn output_profile_identity_is_over_decoded_bytes() {
        // Original synthetic header, not a usable colour conversion profile.
        let profile = format!("{}acsp{}", "0".repeat(36), "0".repeat(88));
        let stream = format!("<< /N 4 /Length {} >>\nstream\n{profile}\nendstream", profile.len());
        let bytes =
            pdf_with_catalog(&[PdfPage::new(72.0, 72.0, "")], &[&stream], "/OutputIntents [<< /S /GTS_PDFX /DestOutputProfile 5 0 R >>]", None);
        let info = production_info(&bytes).unwrap();
        assert_eq!(info.intents.as_ref().unwrap()[0].profile_sha256, Some(format!("{:x}", Sha256::digest(profile.as_bytes()))));
    }
    #[test]
    fn absent_and_empty_intent_arrays_remain_distinct() {
        let page = PdfPage::new(72.0, 72.0, "");
        let absent = production_info(&pdf(std::slice::from_ref(&page), None)).unwrap();
        let mut empty = page;
        empty.entries = "/OutputIntents []".into();
        let empty = production_info(&pdf_with_catalog(&[empty], &[], "/OutputIntents []", None)).unwrap();
        assert!(absent.intents.is_none() && absent.pages[0].intents.is_none());
        assert_eq!(empty.intents, Some(vec![]));
        assert_eq!(empty.pages[0].intents, Some(vec![]));
    }
    #[test]
    fn unfiltered_profile_cannot_exceed_the_decode_budget() {
        let profile = format!("{}acsp{}", "0".repeat(36), "0".repeat(8 * 1024 * 1024));
        let stream = format!("<< /N 4 /Length {} >>\nstream\n{profile}\nendstream", profile.len());
        let bytes =
            pdf_with_catalog(&[PdfPage::new(72.0, 72.0, "")], &[&stream], "/OutputIntents [<< /S /GTS_PDFX /DestOutputProfile 5 0 R >>]", None);
        assert!(production_info(&bytes).unwrap_err().to_string().contains("byte budget"));
    }
    #[test]
    fn compressed_profile_cannot_be_hashed_as_a_truncated_prefix() {
        use std::io::Write;
        let mut profile = vec![0; 8 * 1024 * 1024 + 1];
        profile[36..40].copy_from_slice(b"acsp");
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&profile).unwrap();
        let hex: String = encoder.finish().unwrap().iter().map(|b| format!("{b:02x}")).collect();
        let stream = format!("<< /N 4 /Filter [/ASCIIHexDecode /FlateDecode] /Length {} >>\nstream\n{hex}>\nendstream", hex.len() + 1);
        let bytes =
            pdf_with_catalog(&[PdfPage::new(72.0, 72.0, "")], &[&stream], "/OutputIntents [<< /S /GTS_PDFX /DestOutputProfile 5 0 R >>]", None);
        assert!(production_info(&bytes).unwrap_err().to_string().contains("byte budget"));
    }
    #[test]
    fn dangling_parent_is_not_a_root_and_raw_numbers_keep_f64_precision() {
        let bytes = pdf(&[PdfPage::new(595.276, 841.89, "")], None);
        let page = &production_info(&bytes).unwrap().pages[0];
        assert!((page.media_mm[2] - 595.276 * 25.4 / 72.0).abs() < 1e-12);
        let broken = String::from_utf8(bytes).unwrap().replace("/Parent 2 0 R", "/Parent 9 0 R");
        assert!(production_info(broken.as_bytes()).unwrap_err().to_string().contains("Parent"));
    }
}
