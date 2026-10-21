//! Private, versioned marked-content names. PDF consumers ignore unknown tags.
//! Hex-encoded JSON has no PDF delimiters, so artist-provided list names cannot inject operators.
use crate::PdfError;
use vectorcraft_doc::SepMark;
const PREFIX: &str = "VectorCraftSep1_";
pub(crate) fn tag(mark: &SepMark) -> Result<String, PdfError> {
    if !mark.valid() {
        return Err(PdfError::Write("invalid separation mark".into()));
    }
    let data = serde_json::to_vec(mark).map_err(|e| PdfError::Write(e.to_string()))?;
    Ok(format!("{PREFIX}{}", data.iter().map(|b| format!("{b:02X}")).collect::<String>()))
}
pub(crate) fn parse(tag: &[u8]) -> Option<SepMark> {
    let hex = tag.strip_prefix(PREFIX.as_bytes())?;
    if hex.len() > 8192 || hex.len() % 2 != 0 {
        return None;
    }
    let data: Option<Vec<u8>> = hex.as_chunks::<2>().0.iter().map(|v| u8::from_str_radix(std::str::from_utf8(v).ok()?, 16).ok()).collect();
    let mark: SepMark = serde_json::from_slice(&data?).ok()?;
    mark.valid().then_some(mark)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_color::{Color, Paint};
    use vectorcraft_doc::{Appearance, Document, Node};
    use vectorcraft_geom::{Rect, shapes};
    #[test]
    fn private_tags_roundtrip_public_pdf_without_editing_data() {
        let mut doc = Document::new(100., 100.);
        let layer = doc.default_layer().unwrap();
        for (i, mark) in
            [Some(SepMark::Metallic { list: "(2a)/Set M-2 (NZ_US) objects".into() }), Some(SepMark::NoTrap), None].into_iter().enumerate()
        {
            let id = doc.alloc_id();
            let mut n = Node::path(
                id,
                shapes::rectangle(Rect::new(10. + i as f64 * 25., 10., 30. + i as f64 * 25., 30.)),
                Appearance::basic(Paint::solid(Color::cmyk(0.2, 0.5, 0.3, 0.1)), Paint::solid(Color::BLACK), 2.),
            );
            n.extras_mut().sep_mark = mark;
            doc.insert(Some(layer), i, n).unwrap();
        }
        let bytes = crate::export(&doc, &Default::default()).unwrap();
        assert!(crate::editing(&bytes).is_none());
        let restored = crate::import(&bytes).unwrap();
        let mut marks = vec![];
        restored.walk(|n| {
            if let Some(mark) = &n.extras().sep_mark {
                marks.push(mark.clone());
            }
        });
        assert!(marks.contains(&SepMark::NoTrap), "{marks:?}");
        assert!(marks.contains(&SepMark::Metallic { list: "(2a)/Set M-2 (NZ_US) objects".into() }), "{marks:?}");
        let mut unmarked = 0;
        restored.walk(|n| {
            if n.path_data().is_some() && n.extras().sep_mark.is_none() {
                unmarked += 1;
            }
        });
        assert!(unmarked > 0);
    }
    #[test]
    fn hostile_and_unknown_tags_are_ignored() {
        for tag in [b"VectorCraftSep1_F".as_slice(), b"VectorCraftSep1_zz", b"VectorCraftSep2_00", b"VectorCraftSep1_7B7D"] {
            assert!(parse(tag).is_none());
        }
        for mark in [SepMark::NoTrap, SepMark::Metallic { list: "metal / (ink) # café".into() }] {
            assert_eq!(parse(tag(&mark).unwrap().as_bytes()), Some(mark));
        }
    }
}
