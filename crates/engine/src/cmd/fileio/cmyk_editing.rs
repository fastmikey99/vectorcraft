//! Keep the public PDF's process inks when adopting its editable Illustrator tree.
//!
//! Resource presence is not proof of preservation: a hidden copy, another placement or a mask
//! may contain the same image. Match actual printed occurrences, then give each matched object
//! the public image's bytes. JPEG and lossless editing copies need not have identical samples.
//! Ambiguous placements and compositing we cannot establish safely keep the public PDF instead.

use std::collections::BTreeSet;
use std::sync::Arc;

use vectorcraft_color::BlendMode;
use vectorcraft_doc::{Document, ImageBlob, ImageObject, Knockout, Node, NodeId, NodeKind};
use vectorcraft_geom::{Point, Rect};

struct Placement<'a> {
    id: NodeId,
    image: &'a ImageObject,
    simple: bool,
}

// Bound traversal output for untrusted input.
const MAX_PLACEMENTS: usize = 1024;

fn placements<'a>(nodes: &'a [Arc<Node>], simple: bool, out: &mut Vec<Placement<'a>>) -> Option<()> {
    for node in nodes {
        if !node.visible || node.opacity == 0.0 || matches!(node.kind, NodeKind::Layer { printable: false, .. }) {
            continue;
        }
        let simple = simple
            && node.opacity == 1.0
            && node.blend == BlendMode::Normal
            && !node.isolate
            && node.knockout == Knockout::Neutral
            && !node.knockout_shape
            && node.extras().mask.as_ref().is_none_or(|m| m.disabled)
            && node.appearance.effects.is_empty()
            && node.appearance.items.is_empty();
        match &node.kind {
            NodeKind::Image(image) => {
                if out.len() == MAX_PLACEMENTS {
                    return None;
                }
                out.push(Placement { id: node.id, image, simple });
            }
            NodeKind::Layer { children, clip, .. } | NodeKind::Group { children, clip } => {
                placements(children, simple && !clip, out)?;
            }
            // These can draw image occurrences outside the ordinary tree. Do not guess their
            // placements or treat a definition in the resource table as a printed occurrence.
            NodeKind::SymbolInstance { .. } | NodeKind::Blend { .. } | NodeKind::Envelope { .. } | NodeKind::Repeat(_) => return None,
            _ => {
                if let Some(children) = node.children() {
                    placements(children, false, out)?;
                }
            }
        }
    }
    Some(())
}

fn corners(image: &ImageObject, board: Rect) -> [Point; 4] {
    let (w, h) = (f64::from(image.width), f64::from(image.height));
    [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].map(|p| image.xf * Point::from(p) - board.origin().to_vec2())
}

fn same_place(a: &Placement<'_>, b: &Placement<'_>, boards: [Rect; 2]) -> bool {
    a.image.width == b.image.width
        && a.image.height == b.image.height
        && corners(a.image, boards[0]).iter().zip(corners(b.image, boards[1])).all(|(p, q)| p.distance(q) <= 0.001)
}

pub(super) fn retain_public_inks(page: &Document, editing: &mut Document) -> bool {
    let mut public_ids = BTreeSet::new();
    page.visit_images(|node, im| {
        if page.images.get(&im.key).is_some_and(ImageBlob::is_cmyk) {
            public_ids.insert(node.id);
        }
    });
    if public_ids.is_empty() {
        return true;
    }
    // Public pages can be arranged differently from editable artboards. Until that mapping is
    // unambiguous, retaining the public document is safer than moving/replacing an image by guess.
    let ([p], [e]) = (page.artboards.as_slice(), editing.artboards.as_slice()) else { return false };
    let boards = [p.rect, e.rect];
    if (p.rect.width() - e.rect.width()).abs() > 0.001 || (p.rect.height() - e.rect.height()).abs() > 0.001 {
        return false;
    }
    let (mut source, mut candidates) = (vec![], vec![]);
    if placements(&page.layers, true, &mut source).is_none() || placements(&editing.layers, true, &mut candidates).is_none() {
        return false;
    }
    if !public_ids.iter().all(|id| source.iter().any(|s| s.id == *id)) {
        return false;
    }
    // Keep off-artboard editing objects, but do not let one satisfy an on-page occurrence.
    candidates.retain(|c| {
        let r = c.image.xf.transform_rect_bbox(Rect::new(0.0, 0.0, f64::from(c.image.width), f64::from(c.image.height)));
        r.intersect(boards[1]).area() > 0.0
    });
    if source.len() != candidates.len() {
        return false;
    }
    let mut replacements = vec![];
    // Pair occurrences in paint order, including RGB images. A duplicate CMYK occurrence behind
    // an RGB replacement cannot satisfy this check, nor can one resource stand in for two uses.
    for (source, candidate) in source.iter().zip(&candidates) {
        if !same_place(source, candidate, boards) {
            return false;
        }
        if !page.images.get(&source.image.key).is_some_and(ImageBlob::is_cmyk) {
            continue;
        }
        if !source.simple || !candidate.simple {
            return false;
        }
        let Some(other) = editing.images.get(&candidate.image.key).filter(|im| im.is_cmyk()) else { return false };
        let Some(blob) = page.images.get(&source.image.key) else { return false };
        if blob != other {
            // A private copy can use lossless original pixels while the PDF stores JPEG. Keep
            // the editable object, but bind it to the authoritative public samples/encoding.
            // Never overwrite a shared resource: a hidden/off-artboard object may still use it.
            replacements.push((candidate.id, blob.clone()));
        }
    }
    for (id, blob) in replacements {
        let key = format!("public-cmyk-{}-{}", id.0, blob.content_key());
        // Do not overwrite even an unusually named private resource with different contents.
        if editing.images.get(&key).is_some_and(|existing| existing != &blob) {
            return false;
        }
        let Some(node) = editing.node_mut(id) else { return false };
        let NodeKind::Image(image) = &mut node.kind else { return false };
        image.key = key.clone();
        image.link = None;
        editing.images.insert(key, blob);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_doc::{OpacityMask, cmyk::Inks};
    use vectorcraft_geom::Affine;

    fn sample() -> Document {
        let mut doc = Document::new(100.0, 100.0);
        doc.layers.clear();
        doc.images.insert("ink".into(), ImageBlob::cmyk_tiff(&Inks::new(1, 1, vec![0, 0, 0, 255]).unwrap()).unwrap());
        let image = ImageObject {
            key: "ink".into(),
            width: 1,
            height: 1,
            xf: Affine::new([10.0, 0.0, 0.0, 10.0, 20.0, 20.0]),
            link: None,
            placement: Default::default(),
        };
        doc.layers.push(Arc::new(Node::group(NodeId(1), vec![Arc::new(Node::new(NodeId(2), NodeKind::Image(Box::new(image))))])));
        doc
    }

    #[test]
    fn placement_and_compositing_are_required_not_just_a_resource_match() {
        let page = sample();
        assert!(retain_public_inks(&page, &mut page.clone()));
        for change in ["hidden", "opacity", "group opacity", "blend", "mask", "clip", "moved", "mirrored", "nonprinting", "duplicate"] {
            let mut editing = page.clone();
            let image = editing.node_mut(NodeId(2)).unwrap();
            match change {
                "hidden" => image.visible = false,
                "opacity" => image.opacity = 0.5,
                "blend" => image.blend = BlendMode::Multiply,
                "mask" => image.extras_mut().mask = Some(Box::new(OpacityMask::new(Node::group(NodeId(3), vec![]), true))),
                "moved" | "mirrored" => {
                    let NodeKind::Image(im) = &mut image.kind else { panic!() };
                    im.xf =
                        if change == "moved" { Affine::translate((200.0, 0.0)) * im.xf } else { Affine::new([-10.0, 0.0, 0.0, 10.0, 30.0, 20.0]) };
                }
                "clip" => {
                    let NodeKind::Group { clip, .. } = &mut editing.node_mut(NodeId(1)).unwrap().kind else { panic!() };
                    *clip = true;
                }
                "group opacity" => editing.node_mut(NodeId(1)).unwrap().opacity = 0.5,
                "nonprinting" => {
                    let parent = editing.node_mut(NodeId(1)).unwrap();
                    let children = parent.children().unwrap().clone();
                    parent.kind = NodeKind::Layer {
                        color: vectorcraft_doc::LayerColor::Preset(0),
                        template: false,
                        printable: false,
                        children,
                        clip: false,
                        preview: true,
                        dim_images: None,
                    };
                }
                "duplicate" => {
                    let mut duplicate = editing.node(NodeId(2)).unwrap().clone();
                    duplicate.id = NodeId(3);
                    editing.node_mut(NodeId(1)).unwrap().children_mut().unwrap().push(Arc::new(duplicate));
                }
                _ => panic!("unknown test case"),
            }
            assert!(!retain_public_inks(&page, &mut editing), "{change}");
        }
    }

    #[test]
    fn every_repeated_placement_must_keep_cmyk() {
        let mut page = sample();
        let mut second = page.node(NodeId(2)).unwrap().clone();
        second.id = NodeId(3);
        second.transform(Affine::translate((20.0, 0.0)), false);
        page.node_mut(NodeId(1)).unwrap().children_mut().unwrap().push(Arc::new(second));
        assert!(retain_public_inks(&page, &mut page.clone()));
        let mut editing = page.clone();
        let NodeKind::Image(im) = &mut editing.node_mut(NodeId(3)).unwrap().kind else { panic!() };
        im.key = "rgb".into();
        editing.images.insert("rgb".into(), ImageBlob::png(vec![]));
        assert!(!retain_public_inks(&page, &mut editing));
    }
}
