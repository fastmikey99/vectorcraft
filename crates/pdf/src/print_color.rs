//! Strict named-colour identity for print auditing, separate from tolerant editing.
use craft_pdf::syntax::object::{Array, Name, Object};
use std::collections::BTreeSet;

pub(crate) fn validate_named_space(array: &Array<'_>) -> Result<(), &'static str> {
    let mut it = array.flex_iter();
    let kind = it.next::<Name<'_>>();
    let names = match kind.as_deref() {
        Some(b"Separation") => vec![it.next::<Name<'_>>().ok_or("Missing Separation colourant name")?],
        Some(b"DeviceN") => {
            let names = it.next::<Array<'_>>().ok_or("Missing DeviceN colourant array")?;
            if !crate::import_color::readable(&names) {
                return Err("Malformed DeviceN colourant array");
            }
            let objects: Vec<_> = names.iter::<Object<'_>>().take(33).collect();
            if objects.is_empty() || objects.len() > 32 {
                return Err("DeviceN colourant count exceeds the supported 1–32 range");
            }
            objects
                .into_iter()
                .map(|o| match o {
                    Object::Name(name) => Ok(name),
                    _ => Err("DeviceN colourant array contains a non-name entry"),
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        _ => return Ok(()),
    };
    if names.iter().map(|n| n.as_ref().len()).sum::<usize>() > 64 * 1024 {
        return Err("Named colourant identity exceeds the byte budget");
    }
    let mut seen = BTreeSet::new();
    for name in names {
        let bytes = name.as_ref();
        if bytes.is_empty() || std::str::from_utf8(bytes).is_err() {
            return Err("Colourant name cannot be represented losslessly in the UTF-8 recipe contract");
        }
        // None may repeat in ordinary DeviceN, but remains unresolved by the
        // separate special-colourant gate. Never normalize spelling or spacing.
        if !seen.insert(bytes.to_vec()) && bytes != b"None" {
            return Err("DeviceN contains duplicate decoded colourant names");
        }
    }
    Ok(())
}
