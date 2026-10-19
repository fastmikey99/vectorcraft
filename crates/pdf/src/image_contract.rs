//! Validate image declarations before the tolerant reader normalizes them.
//! ISO 32000-1 sections 7.4 and 8.9: filter order, parameter alignment and
//! integer-typed sample layout must survive into source-sample evidence.
use craft_pdf::syntax::{
    Filter,
    object::{Array, Dict, Name, Number, Object, Stream},
};

pub(crate) fn integer(value: Object<'_>) -> Option<u32> {
    let Object::Number(n) = value else { return None };
    let i = i32::try_from(n.as_i64()).ok()?;
    (n == Number::from_i32(i)).then_some(i).and_then(|i| u32::try_from(i).ok())
}

pub(crate) fn integer_entry(dict: &Dict<'_>, key: &[u8], default: Option<u32>) -> Result<u32, String> {
    if dict.contains_key(key) && dict.get::<Object<'_>>(key).is_none() {
        return Err("Image integer declaration is unreadable".into());
    }
    match dict.get::<Object<'_>>(key) {
        None | Some(Object::Null(_)) => default.ok_or("Image integer declaration is missing".into()),
        Some(value) => integer(value).ok_or("Image integer declaration has an invalid type or value".into()),
    }
}

fn objects<'a>(array: &Array<'a>) -> Result<Vec<Object<'a>>, String> {
    if !crate::import_color::readable(array) {
        return Err("Image filter declaration array is unreadable".into());
    }
    let values: Vec<_> = array.iter::<Object<'_>>().take(17).collect();
    if values.len() != crate::import_color::items(array).take(17).count() {
        return Err("Image filter declaration contains an unresolved reference".into());
    }
    if values.len() > 16 {
        return Err("Image filter chain exceeds declaration budget".into());
    }
    Ok(values)
}

fn named_filter(name: Name<'_>) -> Result<Filter, String> {
    Ok(match name.as_ref() {
        b"ASCIIHexDecode" | b"AHx" => Filter::AsciiHexDecode,
        b"ASCII85Decode" | b"A85" => Filter::Ascii85Decode,
        b"LZWDecode" | b"LZW" => Filter::LzwDecode,
        b"FlateDecode" | b"Fl" => Filter::FlateDecode,
        b"RunLengthDecode" | b"RL" => Filter::RunLengthDecode,
        b"CCITTFaxDecode" | b"CCF" => Filter::CcittFaxDecode,
        b"JBIG2Decode" => Filter::Jbig2Decode,
        b"DCTDecode" | b"DCT" => Filter::DctDecode,
        b"JPXDecode" => Filter::JpxDecode,
        b"Crypt" => return Err("Image encryption filter requires qualified decoding".into()),
        _ => return Err("Unknown image filter cannot establish source samples".into()),
    })
}

fn parameters(filter: Filter, dict: &Dict<'_>, max: usize) -> Result<(), String> {
    if matches!(filter, Filter::FlateDecode | Filter::LzwDecode) {
        let predictor = integer_entry(dict, b"Predictor", Some(1))?;
        let colors = integer_entry(dict, b"Colors", Some(1))?;
        let columns = integer_entry(dict, b"Columns", Some(1))?;
        let bits = integer_entry(dict, b"BitsPerComponent", Some(8))?;
        if !matches!(predictor, 1 | 2 | 10..=15) || !(1..=255).contains(&colors) || columns == 0 || !matches!(bits, 1 | 2 | 4 | 8 | 16) {
            return Err("Invalid image predictor declaration".into());
        }
        if predictor != 1 {
            let row = u64::from(colors).checked_mul(u64::from(columns)).and_then(|n| n.checked_mul(u64::from(bits)));
            if row.is_none_or(|n| n.div_ceil(8) > max as u64) {
                return Err("Image predictor row exceeds sample budget".into());
            }
        }
        if filter == Filter::LzwDecode && integer_entry(dict, b"EarlyChange", Some(1))? > 1 {
            return Err("Invalid image LZW EarlyChange declaration".into());
        }
    }
    Ok(())
}

pub(crate) fn filters(stream: &Stream<'_>, inline: bool, max: usize) -> Result<(), String> {
    let d = stream.dict();
    if !inline && (d.contains_key(b"F") || d.contains_key(b"DP")) {
        return Err("Non-inline image filter aliases require reconciliation".into());
    }
    if inline && ((d.contains_key(b"Filter") && d.contains_key(b"F")) || (d.contains_key(b"DecodeParms") && d.contains_key(b"DP"))) {
        return Err("Ambiguous inline image filter aliases require reconciliation".into());
    }
    let filter_key = if inline && !d.contains_key(b"Filter") { b"F".as_slice() } else { b"Filter" };
    let params_key = if inline && !d.contains_key(b"DecodeParms") { b"DP".as_slice() } else { b"DecodeParms" };
    for key in [filter_key, params_key] {
        if d.contains_key(key) && d.get::<Object<'_>>(key).is_none() {
            return Err("Image filter or DecodeParms declaration is unreadable".into());
        }
    }
    let (declared, array_form) = match d.get::<Object<'_>>(filter_key) {
        None | Some(Object::Null(_)) => (Vec::new(), false),
        Some(Object::Name(name)) => (vec![named_filter(name)?], false),
        Some(Object::Array(a)) => {
            let filters = objects(&a)?
                .into_iter()
                .map(|value| match value {
                    Object::Name(name) => named_filter(name),
                    _ => Err("Image filter array contains a non-name".into()),
                })
                .collect::<Result<Vec<_>, String>>()?;
            (filters, true)
        }
        _ => return Err("Image filter declaration is not a name or array".into()),
    };
    if declared.as_slice() != stream.filters().as_slice() {
        return Err("Image filter declarations differ from the decoder's filter chain".into());
    }
    if declared.iter().any(|f| matches!(f, Filter::DctDecode | Filter::JpxDecode | Filter::Jbig2Decode | Filter::CcittFaxDecode)) {
        return Err("Image compression requires qualified sample decoding".into());
    }
    match d.get::<Object<'_>>(params_key) {
        None | Some(Object::Null(_)) => Ok(()),
        Some(Object::Dict(params)) if !array_form && declared.len() == 1 => {
            parameters(*declared.first().ok_or("Image filter missing")?, &params, max)
        }
        Some(Object::Array(a)) if array_form => {
            let params = objects(&a)?;
            if params.len() != declared.len() {
                return Err("Image filter and DecodeParms arrays have different lengths".into());
            }
            for (filter, params) in declared.into_iter().zip(params) {
                match params {
                    Object::Null(_) => (),
                    Object::Dict(d) => parameters(filter, &d, max)?,
                    _ => return Err("Image DecodeParms array contains a non-dictionary".into()),
                }
            }
            Ok(())
        }
        _ => Err("Image DecodeParms shape does not match its filter declaration".into()),
    }
}
