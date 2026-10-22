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
    fn marked_mesh_children_are_not_batched_without_editing_data() {
        use std::sync::Arc;
        use vectorcraft_doc::{NodeKind, live::GradientMesh};
        let mut doc = Document::new(100., 100.);
        let layer = doc.default_layer().unwrap();
        let mut children = Vec::new();
        for i in 0..2 {
            let mesh =
                GradientMesh::from_surface(1, 1, &|u, v| vectorcraft_geom::Point::new(10. + f64::from(i) * 40. + u * 30., 10. + v * 30.), &|_, _| {
                    Color::cmyk(0., 1., 0., 0.)
                });
            let mut node = Node::new(doc.alloc_id(), NodeKind::Mesh(mesh));
            if i == 0 {
                node.extras_mut().sep_mark = Some(SepMark::NoTrap);
            }
            children.push(Arc::new(node));
        }
        let group = Node::group(doc.alloc_id(), children);
        doc.insert(Some(layer), 0, group).unwrap();
        let bytes = crate::export(&doc, &Default::default()).unwrap();
        assert!(crate::editing(&bytes).is_none());
        let imported = crate::import(&bytes).unwrap();
        let mut marked = Vec::new();
        let mut unmarked = Vec::new();
        imported.walk(|n| {
            if matches!(n.kind, NodeKind::Mesh(_)) {
                if n.extras().sep_mark == Some(SepMark::NoTrap) {
                    marked.push(n.geometric_bounds().unwrap());
                } else {
                    unmarked.push(n.geometric_bounds().unwrap());
                }
            }
        });
        assert!(!marked.is_empty());
        assert!(!unmarked.is_empty());
        assert!(marked.iter().all(|b| b.x1 < 45.));
        assert!(unmarked.iter().all(|b| b.x0 > 45.));
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
