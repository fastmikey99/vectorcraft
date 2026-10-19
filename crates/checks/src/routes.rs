//! Layer labels locate candidates; named inks and recipe output roles identify them.
//! Will's 2026-10-19 clarification: White Underprint numbering is a fingerprint,
//! not a production constraint. Explicit layer_order remains independent of it.
use crate::{pdf::PaintedObject, recipe::FileLink};

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayerLabel<'a> {
    UnderprintFingerprint,
    Exact(&'a str),
}
fn label(value: &str) -> LayerLabel<'_> {
    if value == "White Underprint" || value.strip_prefix("White Underprint-").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        LayerLabel::UnderprintFingerprint
    } else {
        LayerLabel::Exact(value)
    }
}
/// Only the documented numbered family is a hint. Ink names are never folded,
/// trimmed or inferred from a layer label; other layer families stay distinct.
pub(crate) fn layer_is_hint(value: &str) -> bool {
    label(value) == LayerLabel::UnderprintFingerprint
}
pub(crate) fn layer_matches(expected: &str, actual: &str) -> bool {
    label(expected) == label(actual)
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PaintGroup<'a> {
    Hint { mode: &'a str, ink: Option<&'a str> },
    Exact(&'a str),
}
/// Same-role hint contributors share a physical paint comparison even when
/// their fingerprints differ. Conflicting roles are refused separately.
pub(crate) fn paint_group(link: &FileLink) -> PaintGroup<'_> {
    if layer_is_hint(&link.pdf_layer) {
        PaintGroup::Hint { mode: &link.name_mode, ink: link.separation_name.as_deref() }
    } else {
        PaintGroup::Exact(&link.pdf_layer)
    }
}
pub(crate) fn paint_matches(link: &FileLink, paint: &PaintedObject) -> bool {
    layer_matches(&link.pdf_layer, &paint.layer)
        && match link.name_mode.as_str() {
            "source" => true,
            "exact" => link.separation_name.is_some() && paint.ink.as_ref() == link.separation_name.as_ref(),
            _ => false,
        }
}
/// Multiple contributors to one ink/role remain possible, without proving each
/// contribution. A label fingerprint cannot distinguish conflicting roles.
pub(crate) fn ambiguous(links: &[&FileLink], paint: &PaintedObject) -> bool {
    let mut candidates = links.iter().filter(|link| paint_matches(link, paint));
    let Some(first) = candidates.next() else { return false };
    candidates.any(|other| {
        other.destination != first.destination
            || other.name_mode != first.name_mode
            || other.separation_name != first.separation_name
            || other.overprint != first.overprint
            || other.layer_order != first.layer_order
    })
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
