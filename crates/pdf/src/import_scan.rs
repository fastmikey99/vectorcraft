//! What the interpreter doesn't tell the import, read from the file itself: the optional content
//! groups (layers) with their states and nesting, which group each marked-content sequence belongs
//! to, the isolate and knockout flags of transparency groups, the overprint parameters each path
//! and each stretch of text is painted with, the Separation and DeviceN inks they are painted in
//! (by their colorant names, which the interpreter drops), and the names of the fonts.
//!
//! hayro-interpret reports a marked-content sequence by its tag alone and a transparency group
//! without its flags, and skips the content of groups that are off. So each page's content is
//! walked here in the interpreter's order (page content, then form XObjects as they are drawn,
//! skipping what it skips): the k-th `BDC`/`BMC` it reports is the k-th one found here (its tag
//! checks that), the k-th form transparency group is the k-th group found here, and the k-th
//! path it fills or strokes is the k-th one painted here (the page's count checks that). To import
//! content that is off as hidden layers, [`all_on`] gives the file an update that turns every
//! group on.

use std::collections::{HashMap, HashSet};

use crate::import_color::items;
use craft_pdf::interpret::CacheKey;
use craft_pdf::syntax::Pdf;
use craft_pdf::syntax::content::TypedIter;
use craft_pdf::syntax::content::ops::TypedInstruction;
use craft_pdf::syntax::object::{Array, Dict, FromBytes, MaybeRef, Name, Object, ObjectIdentifier, dict_or_stream};
use craft_pdf::syntax::page::{Page, Resources};

use crate::import_color::ink_names;
use crate::{
    PdfError,
    print_order::{PrintColorSpace, PrintPaintSource},
};

/// The interpreter's limit on nested form XObjects.
const MAX_DEPTH: u32 = 50;
/// The most content operators walked per page (a guard against forms drawn within each other
/// many times over): the art past it can't be told apart by group. A drawing of a million paths
/// takes a few million; the walk reads about 15 million a second, twice as fast as the
/// interpreter draws them.
const MAX_OPS: usize = 64_000_000;
/// The most marked-content sequences noted per page (24 bytes each): past it the rest of the
/// page isn't told apart either.
const MAX_TAGS: usize = 4_000_000;
/// The most groups read.
const MAX_GROUPS: usize = 10_000;
/// Match the interpreter's ordinary content window and maximum incomplete instruction.
const CONTENT_WINDOW: usize = 64 << 10;
const MAX_WINDOW: usize = 256 << 20;
/// The most Separation and DeviceN inks noted per page.
const MAX_INKS: usize = 4096;
/// The most changes of ink noted per page in the paths, and in the text (24 bytes each).
const MAX_INK_CHANGES: usize = 2_000_000;
/// The deepest layers nest in the configuration's `/Order`.
pub(crate) const MAX_NESTING: usize = 32;

/// An optional content group: a layer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ocg {
    pub name: String,
    /// A missing/empty name received an editor label, not an exact PDF identity.
    pub synthetic_name: bool,
    /// On in the file's default configuration (shown by viewers).
    pub on: bool,
    /// Printed (its print usage isn't off).
    pub print: bool,
    pub locked: bool,
    /// The group it is listed under in the layer order (a sublayer of it).
    pub parent: Option<usize>,
}

/// The file's optional content groups.
#[derive(Default)]
pub(crate) struct Ocgs {
    pub list: Vec<Ocg>,
    index: HashMap<ObjectIdentifier, usize>,
    /// The groups that are off (as the interpreter decides it).
    off: HashSet<ObjectIdentifier>,
    /// The groups whose view state applies when the file opens (an `/AS` entry for viewing).
    view_auto: HashSet<ObjectIdentifier>,
    /// The groups placed in the layer order.
    placed: HashSet<usize>,
}

impl Ocgs {
    /// The groups listed in the catalog's `/OCProperties`, with their default states.
    pub fn read(pdf: &Pdf) -> Self {
        let mut o = Self::default();
        let xref = pdf.xref();
        let Some(props) = xref.get::<Dict<'_>>(xref.root_id()).and_then(|c| c.get::<Dict<'_>>(b"OCProperties")) else {
            return o;
        };
        let config = props.get::<Dict<'_>>(b"D").unwrap_or_default();
        let refs = |d: &Dict<'_>, key: &[u8]| -> Vec<ObjectIdentifier> {
            d.get::<Array<'_>>(key)
                .map(|a| items(&a).filter_map(|i| i.as_obj_ref()).map(ObjectIdentifier::from).take(MAX_GROUPS).collect())
                .unwrap_or_default()
        };
        let all = refs(&props, b"OCGs");
        if config.get::<Name<'_>>(b"BaseState").is_some_and(|n| n.as_ref() == b"OFF") {
            o.off.extend(all.iter().copied());
        }
        for id in refs(&config, b"ON") {
            o.off.remove(&id);
        }
        o.off.extend(refs(&config, b"OFF"));
        let auto = config.get::<Array<'_>>(b"AS").map(|a| a.iter::<Dict<'_>>().take(MAX_GROUPS).collect::<Vec<_>>()).unwrap_or_default();
        for d in auto.iter().filter(|d| d.get::<Name<'_>>(b"Event").is_some_and(|e| e.as_ref() == b"View")) {
            o.view_auto.extend(refs(d, b"OCGs"));
        }
        let locked: HashSet<_> = refs(&config, b"Locked").into_iter().collect();
        for id in all {
            if let Some(d) = xref.get::<Dict<'_>>(id) {
                o.add(id, &d, locked.contains(&id));
            }
        }
        if let Some(order) = config.get::<Array<'_>>(b"Order") {
            o.nest(&order, None, 0, xref, &mut 0);
        }
        o
    }

    /// Read the layer order `order` (listed under group `parent`): a group followed by an array
    /// lists that array's groups as its sublayers; an array starting with a label lists its
    /// groups under the label, which isn't a group (they go under `parent`). A group listed twice
    /// stays where it is first listed. `seen`: the entries read so far.
    fn nest(&mut self, order: &Array<'_>, parent: Option<usize>, depth: usize, xref: &craft_pdf::syntax::xref::XRef, seen: &mut usize) {
        let mut prev = None;
        for item in items(order) {
            *seen += 1;
            if *seen > 4 * MAX_GROUPS {
                return;
            }
            let sub = match item {
                MaybeRef::Ref(r) => {
                    let id = ObjectIdentifier::from(r);
                    let sub = xref.get::<Array<'_>>(id);
                    if sub.is_none() {
                        prev = self.place(id, parent);
                    }
                    sub
                }
                MaybeRef::NotRef(o) => o.into_array(),
            };
            if let Some(sub) = sub
                && depth < MAX_NESTING
            {
                let label = matches!(items(&sub).next(), Some(MaybeRef::NotRef(Object::String(_))));
                let under = prev.take().filter(|_| !label).or(parent);
                self.nest(&sub, under, depth + 1, xref, seen);
            }
        }
    }

    /// Put group `id` under `parent` in the layer order → its index, unless it isn't a group or
    /// is already placed. Each group is placed after its parent, so the nesting has no cycles.
    fn place(&mut self, id: ObjectIdentifier, parent: Option<usize>) -> Option<usize> {
        let i = *self.index.get(&id)?;
        let placed = self.placed.insert(i);
        let g = self.list.get_mut(i).filter(|_| placed)?;
        g.parent = parent;
        Some(i)
    }

    /// Note group `id` (its dictionary `d`) → its index.
    fn add(&mut self, id: ObjectIdentifier, d: &Dict<'_>, locked: bool) -> usize {
        if let Some(i) = self.index.get(&id) {
            return *i;
        }
        let name = d.get::<craft_pdf::syntax::object::String<'_>>(b"Name").map(|s| text_string(s.as_bytes())).unwrap_or_default();
        let print = usage(d, b"Print", b"PrintState") != Some(false);
        // A view state applies when the configuration says so on opening; an off one hides the
        // group all the same (it says how the group is meant to be seen).
        let view = usage(d, b"View", b"ViewState");
        let on = match view {
            Some(v) if self.view_auto.contains(&id) => v,
            Some(false) => false,
            _ => !self.off.contains(&id),
        };
        let synthetic_name = name.is_empty();
        let name = if synthetic_name { format!("Layer {}", self.list.len() + 1) } else { name };
        self.list.push(Ocg { name, synthetic_name, on, print, locked, parent: None });
        self.index.insert(id, self.list.len() - 1);
        self.list.len() - 1
    }

    /// Does the interpreter leave out the art of some group (off in the default configuration)?
    pub fn skips_any(&self) -> bool {
        self.index.keys().any(|id| self.off.contains(id))
    }

    /// The group content marked with `/OC` (`d`: the group or membership dictionary, `id` its
    /// object) belongs to: a membership dictionary's first group.
    fn group_of(&mut self, d: &Dict<'_>, id: ObjectIdentifier, xref: &craft_pdf::syntax::xref::XRef) -> Option<usize> {
        if d.get::<Name<'_>>(b"Type").is_some_and(|t| t.as_ref() == b"OCMD") {
            let first = d.get::<Array<'_>>(b"OCGs").and_then(|a| items(&a).find_map(|i| i.as_obj_ref())).or_else(|| d.get_ref(b"OCGs"))?;
            let id = ObjectIdentifier::from(first);
            return self.note(id, &xref.get::<Dict<'_>>(id)?);
        }
        self.note(id, d)
    }

    /// The index of group `id` (dictionary `d`), noted if new (up to [`MAX_GROUPS`]).
    fn note(&mut self, id: ObjectIdentifier, d: &Dict<'_>) -> Option<usize> {
        match self.index.get(&id) {
            Some(i) => Some(*i),
            None => (self.list.len() < MAX_GROUPS).then(|| self.add(id, d, false)),
        }
    }
}

/// The state (`ON` → true, `OFF` → false) group `d`'s usage `category` gives in `key`.
fn usage(d: &Dict<'_>, category: &[u8], key: &[u8]) -> Option<bool> {
    let state = d.get::<Dict<'_>>(b"Usage")?.get::<Dict<'_>>(category)?.get::<Name<'_>>(key)?;
    match state.as_ref() {
        b"ON" => Some(true),
        b"OFF" => Some(false),
        _ => None,
    }
}

/// A PDF text string: UTF-16 (with its byte order mark), UTF-8 (with one) or PDFDocEncoding
/// (read as Latin-1, which it matches for letters).
pub(crate) fn text_string(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    b.iter().map(|&c| c as char).collect()
}

/// One page as the interpreter walks it.
#[derive(Default)]
pub(crate) struct Scan {
    /// Each marked-content sequence: its tag ([`tag_key`]) and the group (index into
    /// [`Ocgs::list`]) it marks.
    pub tags: Vec<(u64, Option<usize>)>,
    /// Each form transparency group: isolated, knockout.
    pub groups: Vec<(bool, bool)>,
    /// Content was incomplete or a scan limit was reached. No document may be returned from
    /// this scan, since its routing no longer describes every object the interpreter draws.
    pub cut: bool,
    /// Each fill and each stroke of a path the interpreter draws (a fill-and-stroke operator is
    /// a fill, then a stroke; `sh` is a fill): whether it overprints.
    pub paints: Vec<bool>,
    /// Overprint mode of each path paint, parallel to paints.
    pub paint_zero: Vec<bool>,
    /// Optional production audit; ordinary editable imports allocate no event log.
    pub print_order: Option<crate::print_order::PageOrder>,
    /// How the text shown before each of [`Self::paints`] (since the one before it) overprints,
    /// and last the text after them all: one more entry than `paints`.
    pub texts: Vec<TextOverprint>,
    /// Something on the page overprints (when nothing does, the overprint flags in `paints` and
    /// `texts` aren't read).
    pub overprints: bool,
    /// Something overprints with overprint mode 0, where a `DeviceCMYK` colour's zero components
    /// knock out; the document's overprint is the nonzero one (mode 1).
    pub zero_mode: bool,
    /// The colorant names of the Separation and DeviceN spaces the page sets (`cs`, `CS`), and of
    /// those of the shading patterns it sets (`scn`, `SCN`) and of the shadings it draws (`sh`),
    /// each once (none when the page uses more than [`MAX_INKS`] inks or changes ink more than
    /// [`MAX_INK_CHANGES`] times).
    pub inks: Vec<Vec<String>>,
    /// The inks each of [`Self::paints`] is painted in.
    paint_inks: Changes<Inks>,
    /// The inks of the text before each of [`Self::paints`], counted as [`Self::texts`] is: of its
    /// fills and of its strokes (indices into [`Self::inks`]), where all of its text shown in an
    /// ink is shown in the same one.
    text_inks: Changes<[Option<u32>; 2]>,
}

impl Scan {
    /// The colorant names of the ink the walk found `at` painted in: a path's, or the fills' (the
    /// `stroke`s') of the text before a path. `pattern`: the interpreter paints with a pattern,
    /// whose shading's ink it is (text painted with one isn't followed); else the color space's.
    pub fn ink(&self, at: Walked, stroke: bool, pattern: bool) -> Option<&[String]> {
        let i = match at {
            Walked::Path(k) => {
                let inks = self.paint_inks.at(k);
                if pattern { inks.pattern } else { inks.space }
            }
            Walked::Text(_) if pattern => None,
            Walked::Text(k) => self.text_inks.at(k)[usize::from(stroke)],
        }?;
        self.inks.get(usize::try_from(i).ok()?).map(Vec::as_slice)
    }
}

/// The inks a fill or a stroke paints in (indices into [`Scan::inks`]): that of the color space
/// set last (`cs`, `CS`), and that of the shading of the pattern named last (`scn`, `SCN`). The
/// interpreter's paint, a color or a pattern, says which of the two it is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Inks {
    space: Option<u32>,
    pattern: Option<u32>,
}

/// What the interpreter draws, counted as the walk counts it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Walked {
    /// The `k`-th path ([`Scan::paints`]).
    Path(usize),
    /// The text shown before the `k`-th path ([`Scan::texts`]).
    Text(usize),
}

/// A value for each index from 0, kept as the indices where it changes (the default before the
/// first).
#[derive(Default)]
struct Changes<T>(Vec<(usize, T)>);

impl<T: Copy + Default + PartialEq> Changes<T> {
    /// The value is `v` from index `at` on (`at` past the indices set before). `false`: the value
    /// changes there and [`MAX_INK_CHANGES`] changes are kept already, so this one isn't kept.
    fn set(&mut self, at: usize, v: T) -> bool {
        if self.0.last().map_or(T::default(), |(_, last)| *last) == v {
            return true;
        }
        if self.0.len() >= MAX_INK_CHANGES {
            return false;
        }
        self.0.push((at, v));
        true
    }

    /// The value at index `k`.
    fn at(&self, k: usize) -> T {
        let n = self.0.partition_point(|(at, _)| *at <= k);
        n.checked_sub(1).and_then(|i| self.0.get(i)).map_or(T::default(), |(_, v)| *v)
    }
}

/// The ink of the text shown between two painted paths: of its fills, or of its strokes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum RunInk {
    /// No text is shown in an ink.
    #[default]
    None,
    /// All of the text shown in an ink is shown in this one.
    One(u32),
    /// Text is shown in several inks, which can't be told apart by glyph.
    Several,
}

impl RunInk {
    /// With more text shown in `ink` (`None`: in no ink).
    fn and(self, ink: Option<u32>) -> Self {
        match (self, ink) {
            (_, None) => self,
            (Self::None, Some(i)) => Self::One(i),
            (Self::One(j), Some(i)) if i == j => self,
            _ => Self::Several,
        }
    }

    fn ink(self) -> Option<u32> {
        match self {
            Self::One(i) => Some(i),
            _ => None,
        }
    }
}

/// How the text shown between two painted paths overprints.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum TextOverprint {
    /// No text is shown there.
    #[default]
    None,
    /// All of it is shown with these parameters.
    All { fill: bool, stroke: bool },
    /// Text is shown with different parameters, which can't be told apart by glyph.
    Mixed,
}

/// What the walk follows of the graphics state: the overprint parameters (ISO 32000-1 §8.6.7;
/// `OP`, `op` and `OPM` in Table 58) and the inks fills and strokes paint in.
#[derive(Clone, Copy, Default)]
struct State {
    stroke: bool,
    fill: bool,
    /// Overprint mode 1 (nonzero overprint).
    nonzero: bool,
    /// The inks of fills and of strokes.
    inks: [Inks; 2],
    spaces: [PrintColorSpace; 2],
    matrix: Option<vectorcraft_geom::Affine>,
    clip_context: bool,
    image_compositing_unresolved: bool,
    source_effects_unresolved: bool,
}

/// A marked-content tag as [`Scan::tags`] keeps it: its FNV-1a hash (the tags are only compared,
/// and a page can have millions).
pub(crate) fn tag_key(tag: &[u8]) -> u64 {
    tag.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// [`MAX_OPS`], lowered by tests that need a page past it.
#[cfg(not(test))]
fn max_ops() -> usize {
    MAX_OPS
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_MAX_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_OPS) };
}

#[cfg(test)]
fn max_ops() -> usize {
    TEST_MAX_OPS.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn max_window() -> usize {
    MAX_WINDOW
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_MAX_WINDOW: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_WINDOW) };
}

#[cfg(test)]
fn max_window() -> usize {
    TEST_MAX_WINDOW.with(std::cell::Cell::get)
}

pub(crate) fn incomplete_content() -> PdfError {
    PdfError::Parse("PDF content is incomplete or exceeds the reader's limits; no document was imported".into())
}

/// Whitespace and comments after the final instruction do not make an incomplete tail.
fn blank_tail(mut bytes: &[u8]) -> bool {
    loop {
        let first = bytes.iter().position(|b| !matches!(b, 0 | 9 | 10 | 12 | 13 | 32)).unwrap_or(bytes.len());
        bytes = bytes.get(first..).unwrap_or_default();
        if bytes.first() == Some(&b'%') {
            let end = bytes.iter().position(|b| matches!(b, b'\r' | b'\n')).unwrap_or(bytes.len());
            bytes = bytes.get(end..).unwrap_or_default();
        } else {
            return bytes.is_empty();
        }
    }
}

struct Walker<'w, 'p> {
    ocgs: &'w mut Ocgs,
    /// The groups that are off for the interpreter (none once [`all_on`] turned them on).
    off: HashSet<ObjectIdentifier>,
    xref: &'p craft_pdf::syntax::xref::XRef,
    fonts: &'w mut FontNames,
    visible: Vec<bool>,
    layers: Vec<Option<usize>>,
    marked_floor: usize,
    ops: usize,
    max_ops: usize,
    max_form_bytes: usize,
    out: Scan,
    /// A form XObject hidden by its own optional content was met.
    hidden_form: bool,
    /// The saved graphics states' overprint parameters and inks, the current one last, pushed and
    /// popped as the interpreter does its states.
    states: Vec<State>,
    /// The text shown since the last painted path.
    text: TextOverprint,
    /// The inks of the text shown since the last painted path: of its fills, of its strokes.
    text_inks: [RunInk; 2],
    /// The index in [`Scan::inks`] of each list of colorant names.
    ink_index: HashMap<Vec<String>, u32>,
    /// More inks or changes of ink than are kept: the page's inks are left out.
    inks_cut: bool,
    print_sources: HashMap<(PrintColorSpace, Option<u32>, bool, bool, u8), usize>,
}

impl Walker<'_, '_> {
    fn is_visible(&self) -> bool {
        self.visible.last().copied().unwrap_or(true)
    }

    fn begin(&mut self, on: bool) {
        let v = self.is_visible() && on;
        self.visible.push(v);
        if self.out.print_order.is_some() {
            self.layers.push(self.layers.last().copied().flatten());
        }
    }

    /// Enter optional content `d` (object `id`): a group, or a membership dictionary whose policy
    /// decides from its groups.
    fn begin_oc(&mut self, d: &Dict<'_>, id: Option<ObjectIdentifier>) {
        if !d.get::<Name<'_>>(b"Type").is_some_and(|t| matches!(t.as_ref(), b"OCG" | b"OCMD")) {
            self.order_issue("Invalid optional-content dictionary cannot establish print order");
        }
        let membership = id.is_none() || d.get::<Name<'_>>(b"Type").is_some_and(|t| t.as_ref() == b"OCMD");
        let on = match id {
            Some(id) if !membership => !self.off.contains(&id),
            _ => {
                let ids: Vec<ObjectIdentifier> = match d.get::<Array<'_>>(b"OCGs") {
                    Some(a) => items(&a).filter_map(|i| i.as_obj_ref()).map(ObjectIdentifier::from).collect(),
                    None => d.get_ref(b"OCGs").map(ObjectIdentifier::from).into_iter().collect(),
                };
                let on = |i: &ObjectIdentifier| !self.off.contains(i);
                ids.is_empty()
                    || match d.get::<Name<'_>>(b"P").as_deref() {
                        Some(b"AllOn") => ids.iter().all(on),
                        Some(b"AnyOff") => !ids.iter().all(on),
                        Some(b"AllOff") => !ids.iter().any(on),
                        _ => ids.iter().any(on),
                    }
            }
        };
        self.begin(on);
        if self.out.print_order.is_some() {
            let group = id.and_then(|id| self.ocgs.group_of(d, id, self.xref));
            if membership {
                self.order_issue("Optional-content membership print state requires verification");
            }
            if let Some(layer) = self.layers.last_mut() {
                *layer = group;
            }
        }
    }
    fn order_issue(&mut self, issue: &str) {
        if let Some(order) = &mut self.out.print_order {
            order.issues.insert(issue.into());
        }
    }
    fn image_mark(&mut self, stream: &craft_pdf::syntax::object::Stream<'_>, res: &Resources<'_>, inline: bool) {
        if self.out.print_order.is_none() || !self.is_visible() {
            return;
        }
        if self.out.print_order.as_ref().is_some_and(|o| o.images.len() >= crate::print_image::MAX_IMAGES) {
            self.out.cut = true;
            return;
        }
        let state = self.state();
        let description = crate::print_image::describe(stream, res, inline);
        if let Some(order) = &mut self.out.print_order {
            let remaining = crate::image_samples::SAMPLE_BUDGET.saturating_sub(order.sample_cost);
            let mut budget = remaining;
            let decoded = crate::image_samples::decode(stream, res, inline, &description, &mut budget);
            order.sample_cost += remaining - budget;
            let (samples, sample_error) = match decoded {
                Ok(samples) => (Some(samples), None),
                Err(error) => (None, Some(error)),
            };
            order.issues.insert("Image sample, mask and compositing coverage requires independent reconciliation".into());
            order.issues.extend(description.issues.iter().cloned());
            if description.optional_content {
                order.issues.insert("Image optional-content print state requires verification".into());
            }
            if state.matrix.is_none() {
                order.issues.insert("Image placement transform is unresolved".into());
            }
            order.images.push(crate::PrintImageUse {
                samples,
                sample_error,
                compositing_unresolved: state.image_compositing_unresolved,
                layer: self.layers.last().copied().flatten(),
                before_path: order.layers.len(),
                inline,
                matrix: state.matrix.map(|m| m.as_coeffs()),
                clip_context: state.clip_context,
                overprint: state.fill,
                overprint_mode: u8::from(state.nonzero),
                description,
            });
        }
    }
    fn concat(&mut self, matrix: [f64; 6]) {
        if self.out.print_order.is_none() {
            return;
        }
        if let Some(s) = self.states.last_mut() {
            s.matrix = s
                .matrix
                .map(|m| m * vectorcraft_geom::Affine::new(matrix))
                .filter(|m| m.as_coeffs().iter().all(|v| v.is_finite() && v.abs() <= 1.0e12));
        }
    }
    fn order_mark(&mut self, stroke: bool) {
        if !self.is_visible() {
            return;
        }
        let state = self.state();
        if state.source_effects_unresolved {
            self.order_issue("Paint transfer, halftone, black generation or rendering intent requires qualification");
        }
        let kind = state.spaces[usize::from(stroke)];
        let ink = state.inks[usize::from(stroke)].space;
        let overprint = if stroke { state.stroke } else { state.fill };
        let overprint_mode = u8::from(state.nonzero);
        if let Some(order) = &mut self.out.print_order {
            let source = if let Some(&i) = self.print_sources.get(&(kind, ink, stroke, overprint, overprint_mode)) {
                i
            } else {
                let names = ink.and_then(|i| self.out.inks.get(i as usize)).map(Vec::as_slice).unwrap_or_default();
                if order.sources.len() >= 4096 || names.len() > 32 || names.iter().map(String::len).sum::<usize>() > 64 * 1024 {
                    self.out.cut = true;
                    return;
                }
                if names.iter().any(|name| matches!(name.as_str(), "All" | "None")) {
                    order.issues.insert("Reserved All/None colourant coverage requires print-semantic reconciliation".into());
                }
                let i = order.sources.len();
                order.sources.push(PrintPaintSource { stroke, overprint, overprint_mode, color_space: kind, colorants: names.to_vec() });
                self.print_sources.insert((kind, ink, stroke, overprint, overprint_mode), i);
                i
            };
            if order.layers.len() >= 50_000 {
                self.out.cut = true;
                return;
            }
            order.layers.push(self.layers.last().copied().flatten());
            order.source_indices.push(source);
        }
    }

    fn state(&self) -> State {
        self.states.last().copied().unwrap_or_default()
    }

    fn save(&mut self) {
        self.states.push(self.state());
    }

    /// The interpreter never pops its first state.
    fn restore(&mut self) {
        if self.states.len() > 1 {
            self.states.pop();
        }
    }

    fn overprinting(&mut self, on: bool) {
        self.out.overprints |= on;
        self.out.zero_mode |= on && !self.state().nonzero;
    }

    /// The interpreter draws a path: a fill (or a `stroke`) of the current one.
    fn painted(&mut self, stroke: bool) {
        if !self.is_visible() {
            return;
        }
        let s = self.state();
        let on = if stroke { s.stroke } else { s.fill };
        self.overprinting(on);
        self.order_mark(stroke);
        let k = self.out.paints.len();
        self.out.paints.push(on);
        self.out.paint_zero.push(!s.nonzero);
        self.out.texts.push(std::mem::take(&mut self.text));
        self.inks_cut |= !self.out.paint_inks.set(k, s.inks[usize::from(stroke)]);
        self.inks_cut |= !self.out.text_inks.set(k, std::mem::take(&mut self.text_inks).map(RunInk::ink));
    }

    /// The interpreter shows text.
    fn shown(&mut self) {
        self.order_issue("Text rendering mode and glyph paint order require direct text census");
        if !self.is_visible() {
            return;
        }
        let s = self.state();
        self.overprinting(s.fill || s.stroke);
        let now = TextOverprint::All { fill: s.fill, stroke: s.stroke };
        self.text = if self.text == TextOverprint::None || self.text == now { now } else { TextOverprint::Mixed };
        // Text painted with a pattern isn't followed, so only the color spaces' inks count.
        for (run, inks) in self.text_inks.iter_mut().zip(s.inks) {
            *run = run.and(inks.space);
        }
    }

    /// The color space of fills (or of `stroke`s) is in ink `ink` from now on.
    fn set_space(&mut self, stroke: bool, ink: Option<u32>, kind: PrintColorSpace) {
        if let Some(s) = self.states.last_mut() {
            s.inks[usize::from(stroke)].space = ink;
            s.spaces[usize::from(stroke)] = kind;
        }
    }

    /// The pattern of fills (or of `stroke`s) is in ink `ink` from now on.
    fn set_pattern(&mut self, stroke: bool, ink: Option<u32>) {
        self.order_issue("Pattern or shading content requires nested composited paint census");
        if let Some(s) = self.states.last_mut() {
            s.inks[usize::from(stroke)].pattern = ink;
            s.spaces[usize::from(stroke)] = PrintColorSpace::Other;
        }
    }

    fn device_space(name: &[u8], res: &Resources<'_>) -> PrintColorSpace {
        let (kind, fallback) = match name {
            b"DeviceCMYK" | b"CMYK" => (PrintColorSpace::DeviceCmyk, b"DefaultCMYK".as_slice()),
            b"DeviceGray" | b"G" => (PrintColorSpace::DeviceGray, b"DefaultGray".as_slice()),
            b"DeviceRGB" | b"RGB" => (PrintColorSpace::DeviceRgb, b"DefaultRGB".as_slice()),
            _ => return PrintColorSpace::Other,
        };
        if Name::new(fallback).and_then(|n| res.get_color_space(&n)).is_some() { PrintColorSpace::Other } else { kind }
    }
    fn source_space(name: &Name<'_>, res: &Resources<'_>) -> PrintColorSpace {
        if matches!(name.as_ref(), b"DeviceCMYK" | b"CMYK" | b"DeviceGray" | b"G" | b"DeviceRGB" | b"RGB") {
            return Self::device_space(name.as_ref(), res);
        }
        match res.get_color_space(name) {
            Some(Object::Name(n)) => Self::device_space(n.as_ref(), res),
            Some(Object::Array(a)) => {
                let mut it = a.flex_iter();
                match it.next::<Name<'_>>().as_deref() {
                    Some(b"Separation") => PrintColorSpace::Separation,
                    Some(b"DeviceN") => PrintColorSpace::DeviceN,
                    _ => PrintColorSpace::Other,
                }
            }
            _ => PrintColorSpace::Other,
        }
    }

    /// The ink of color space `name` (`cs`, `CS`): a Separation or DeviceN space of the resources.
    /// The names the interpreter reads as device and pattern spaces come first, as it reads them.
    fn space_ink(&mut self, name: &Name<'_>, res: &Resources<'_>) -> Option<u32> {
        if matches!(name.as_ref(), b"DeviceRGB" | b"RGB" | b"DeviceGray" | b"G" | b"DeviceCMYK" | b"CMYK" | b"CalCMYK" | b"Pattern") {
            return None;
        }
        self.ink_of(&res.get_color_space(name)?)
    }

    /// The ink pattern `name` paints in (`scn`, `SCN`): a shading pattern's (a dictionary, where a
    /// tiling pattern is a stream) is its shading's.
    fn pattern_ink(&mut self, name: &Name<'_>, res: &Resources<'_>) -> Option<u32> {
        let Object::Dict(p) = res.get_pattern(name)? else { return None };
        self.shading_ink(&p.get::<Object<'_>>(b"Shading")?)
    }

    /// The ink of a shading (a dictionary or a stream): its color space's.
    fn shading_ink(&mut self, shading: &Object<'_>) -> Option<u32> {
        let (d, _) = dict_or_stream(shading)?;
        self.ink_of(&d.get::<Object<'_>>(b"ColorSpace")?)
    }

    /// The index in [`Scan::inks`] of color space `o` when it is a Separation or DeviceN array, its
    /// colorant names noted when new.
    fn ink_of(&mut self, o: &Object<'_>) -> Option<u32> {
        let Object::Array(a) = o else { return None };
        let (_, names) = ink_names(&mut a.flex_iter())?;
        if let Some(i) = self.ink_index.get(&names) {
            return Some(*i);
        }
        if self.out.inks.len() >= MAX_INKS {
            self.inks_cut = true;
            return None;
        }
        let i = u32::try_from(self.out.inks.len()).ok()?;
        self.ink_index.insert(names.clone(), i);
        self.out.inks.push(names);
        Some(i)
    }

    fn note_fonts(&mut self, res: &Resources<'_>) {
        for key in res.fonts.keys() {
            if let Some(f) = res.fonts.get::<Dict<'_>>(key.as_ref())
                && let Some(name) = f.get::<Name<'_>>(b"BaseFont")
            {
                self.fonts.entry(f.cache_key()).or_insert_with(|| (String::from_utf8_lossy(name.as_ref()).into_owned(), embeds_glyphs(&f)));
            }
        }
    }

    fn walk(&mut self, data: &[u8], res: &Resources<'_>, depth: u32) {
        let mut iter = TypedIter::new(data);
        self.note_fonts(res);
        // The interpreter saves the state for each content stream, and restores what the stream
        // left saved when it ends.
        let saved = self.states.len();
        self.save();
        let marked = self.layers.len();
        let outer = self.out.print_order.is_some().then(|| (self.visible.clone(), self.layers.clone()));
        let floor = self.marked_floor;
        self.marked_floor = marked;
        self.walk_ops(&mut iter, data, res, depth);
        if self.out.print_order.is_some() && self.layers.len() != marked {
            self.order_issue("Unbalanced form marked content cannot establish print order");
            if let Some((visible, layers)) = outer {
                self.layers = layers;
                self.visible = visible;
            }
        }
        self.marked_floor = floor;
        self.out.cut |= !blank_tail(data.get(iter.committed()..).unwrap_or_default());
        self.states.truncate(saved);
    }

    /// Scan the same stream windows as the interpreter without materializing the page.
    /// Graphics and optional-content state live across windows; page state is saved once.
    fn page(&mut self, page: &Page<'_>) {
        let resources = page.resources();
        if let Some(state) = self.states.last_mut() {
            state.spaces = [Self::device_space(b"DeviceGray", resources); 2];
        }
        self.note_fonts(resources);
        let saved = self.states.len();
        self.save();
        let mut content = page.content_reader();
        let mut window = Vec::new();
        let limit = max_window();
        let mut exhausted = false;
        loop {
            let (committed, needs_more) = {
                let mut ops = TypedIter::new_window(&window, exhausted);
                self.walk_ops(&mut ops, &window, resources, 0);
                (ops.committed(), ops.needs_more())
            };
            if committed > window.len() {
                self.out.cut = true;
                break;
            }
            window.drain(..committed);
            if self.out.cut {
                break;
            }
            if !needs_more || exhausted {
                // No success on an undecidable tail or on a parser stop before the source ends.
                // The typed iterator does not expose every malformed-operator failure.
                self.out.cut |= !exhausted || !blank_tail(&window);
                break;
            }
            if window.len() >= limit {
                self.out.cut = true;
                break;
            }
            let want = window.len().max(CONTENT_WINDOW).min(limit.saturating_sub(window.len()));
            if window.try_reserve(want).is_err() {
                self.out.cut = true;
                break;
            }
            if content.read_into(&mut window, want) == 0 {
                exhausted = true;
            }
        }
        self.out.cut |= content.truncated();
        self.states.truncate(saved);
    }

    fn walk_ops(&mut self, iter: &mut TypedIter<'_>, data: &[u8], res: &Resources<'_>, depth: u32) {
        loop {
            let start = iter.committed();
            let Some(op) = iter.next() else {
                break;
            };
            let colour = self.out.print_order.as_ref().and_then(|_| colour_operands(&op));
            if self.out.cut {
                return;
            }
            self.ops += 1;
            if self.ops > self.max_ops || self.out.tags.len() >= MAX_TAGS {
                self.out.cut = true;
                return;
            }
            match op {
                TypedInstruction::BeginMarkedContentWithProperties(bdc) => {
                    let oc = match bdc.1 {
                        Object::Name(n) => {
                            res.properties.get_ref(n.as_ref()).map(|r| (res.properties.get::<Dict<'_>>(n.as_ref()).unwrap_or_default(), r))
                        }
                        o => dict_or_stream(o)
                            .and_then(|(props, _)| props.get_ref(b"OC").map(|r| (props.get::<Dict<'_>>(b"OC").unwrap_or_default(), r))),
                    };
                    let group = match oc {
                        Some((d, r)) => {
                            let id = ObjectIdentifier::from(r);
                            self.begin_oc(&d, Some(id));
                            self.ocgs.group_of(&d, id, self.xref)
                        }
                        None => {
                            if bdc.0.as_ref() == b"OC" {
                                self.order_issue("Unresolved optional-content property cannot establish print order");
                            }
                            self.begin(true);
                            None
                        }
                    };
                    self.out.tags.push((tag_key(bdc.0.as_ref()), group));
                }
                TypedInstruction::BeginMarkedContent(bmc) => {
                    self.begin(true);
                    self.out.tags.push((tag_key(bmc.0.as_ref()), None));
                }
                TypedInstruction::EndMarkedContent(_) => {
                    if self.layers.len() <= self.marked_floor {
                        self.order_issue("Unmatched marked-content end cannot establish print order");
                    }
                    self.visible.pop();
                    self.layers.pop();
                }
                TypedInstruction::Transform(m) => self.concat([m.0.as_f64(), m.1.as_f64(), m.2.as_f64(), m.3.as_f64(), m.4.as_f64(), m.5.as_f64()]),
                TypedInstruction::ClipNonZero(_) | TypedInstruction::ClipEvenOdd(_) => {
                    if let Some(s) = self.states.last_mut() {
                        s.clip_context = true;
                    }
                }
                TypedInstruction::SaveState(_) => self.save(),
                TypedInstruction::RestoreState(_) => self.restore(),
                TypedInstruction::SetGraphicsState(gs) => {
                    if let Some(d) = res.get_ext_g_state(gs.0)
                        && let Some(s) = self.states.last_mut()
                    {
                        let stroke = d.get::<bool>(b"OP");
                        s.stroke = stroke.unwrap_or(s.stroke);
                        // Without `/op`, `/OP` sets both.
                        s.fill = d.get::<bool>(b"op").or(stroke).unwrap_or(s.fill);
                        s.nonzero = d.get::<u8>(b"OPM").map_or(s.nonzero, |m| m != 0);
                        // ISO 32000-1 Table 58: Default restores the page-start
                        // device state. It adds no effect to the predictor's existing
                        // initial-device assumption; runtime qualification is separate.
                        let unchanged_device_state = |key: &[u8]| {
                            d.get::<Name<'_>>(key).is_some_and(|name| {
                                (matches!(key, b"TR" | b"TR2") && name.as_ref() == b"Identity")
                                    || (matches!(key, b"TR2" | b"BG2" | b"UCR2" | b"HT") && name.as_ref() == b"Default")
                            })
                        };
                        s.source_effects_unresolved |= [b"TR".as_slice(), b"TR2", b"BG", b"BG2", b"UCR", b"UCR2", b"HT", b"HTP", b"RI"]
                            .iter()
                            .any(|key| d.contains_key(key) && !unchanged_device_state(key));
                        s.image_compositing_unresolved |=
                            [b"ca".as_slice(), b"CA", b"BM", b"SMask", b"TR", b"TR2", b"BG", b"BG2", b"UCR", b"UCR2", b"HT", b"HTP", b"RI", b"AIS"]
                                .iter()
                                .any(|key| d.contains_key(key) && !unchanged_device_state(key));
                    }
                }
                TypedInstruction::RenderingIntent(_) => {
                    if let Some(s) = self.states.last_mut() {
                        s.image_compositing_unresolved = true;
                        s.source_effects_unresolved = true;
                    }
                }
                TypedInstruction::FillPathEvenOdd(_) | TypedInstruction::FillPathNonZero(_) | TypedInstruction::FillPathNonZeroCompatibility(_) => {
                    self.painted(false);
                }
                TypedInstruction::StrokePath(_) | TypedInstruction::CloseAndStrokePath(_) => self.painted(true),
                TypedInstruction::FillAndStrokeEvenOdd(_)
                | TypedInstruction::FillAndStrokeNonZero(_)
                | TypedInstruction::CloseFillAndStrokeEvenOdd(_)
                | TypedInstruction::CloseFillAndStrokeNonZero(_) => {
                    self.painted(false);
                    self.painted(true);
                }
                // Drawn when the interpreter can read the shading (the page's count tells), as a
                // fill in a state of its own, in the shading's ink.
                TypedInstruction::Shading(s) => {
                    if let Some(sh) = res.get_shading(s.0) {
                        let ink = self.shading_ink(&sh);
                        self.save();
                        self.set_pattern(false, ink);
                        self.painted(false);
                        self.restore();
                    } else {
                        self.order_issue("Missing shading resource cannot establish print completeness");
                    }
                }
                TypedInstruction::ColorSpaceNonStroke(cs) => {
                    let ink = self.space_ink(cs.0, res);
                    self.set_space(false, ink, Self::source_space(cs.0, res));
                }
                TypedInstruction::ColorSpaceStroke(cs) => {
                    let ink = self.space_ink(cs.0, res);
                    self.set_space(true, ink, Self::source_space(cs.0, res));
                }
                // A pattern by name; without one, `scn` sets a color in the space set before.
                TypedInstruction::NonStrokeColorNamed(c) => {
                    if let Some(name) = c.1 {
                        let ink = self.pattern_ink(name, res);
                        self.set_pattern(false, ink);
                    }
                }
                TypedInstruction::StrokeColorNamed(c) => {
                    if let Some(name) = c.1 {
                        let ink = self.pattern_ink(name, res);
                        self.set_pattern(true, ink);
                    }
                }
                TypedInstruction::NonStrokeColorDeviceGray(_) => self.set_space(false, None, Self::device_space(b"DeviceGray", res)),
                TypedInstruction::NonStrokeColorDeviceRgb(_) => self.set_space(false, None, Self::device_space(b"DeviceRGB", res)),
                TypedInstruction::NonStrokeColorCmyk(_) => self.set_space(false, None, Self::device_space(b"DeviceCMYK", res)),
                TypedInstruction::StrokeColorDeviceGray(_) => self.set_space(true, None, Self::device_space(b"DeviceGray", res)),
                TypedInstruction::StrokeColorDeviceRgb(_) => self.set_space(true, None, Self::device_space(b"DeviceRGB", res)),
                TypedInstruction::StrokeColorCmyk(_) => self.set_space(true, None, Self::device_space(b"DeviceCMYK", res)),
                TypedInstruction::ShowText(_)
                | TypedInstruction::ShowTexts(_)
                | TypedInstruction::NextLineAndShowText(_)
                | TypedInstruction::ShowTextWithParameters(_) => self.shown(),
                TypedInstruction::InlineImage(image) => self.image_mark(image.0, res, true),
                TypedInstruction::XObject(x) => {
                    if !self.is_visible() {
                        continue;
                    }
                    let Some(s) = res.get_x_object(x.0) else {
                        self.order_issue("Missing XObject cannot establish print completeness");
                        continue;
                    };
                    let d = s.dict();
                    if d.get::<Name<'_>>(b"Subtype").as_deref() == Some(b"Image") {
                        // Own image optional content is separate from the enclosing marked-content layer.
                        // Preserve the normal import scanner's behaviour; this is raw audit evidence only.
                        if self.out.print_order.is_some() {
                            let oc = d.get::<Dict<'_>>(b"OC");
                            if let Some(oc) = &oc {
                                self.begin_oc(oc, d.get_ref(b"OC").map(ObjectIdentifier::from));
                            }
                            self.image_mark(&s, res, false);
                            if oc.is_some() {
                                self.visible.pop();
                                self.layers.pop();
                            }
                        }
                        continue;
                    }
                    if d.get::<Name<'_>>(b"Subtype").as_deref() != Some(b"Form") || d.get::<[f32; 4]>(b"BBox").is_none() {
                        self.order_issue("Unrecognised XObject requires direct paint census");
                        continue;
                    }
                    if depth >= MAX_DEPTH {
                        self.out.cut = true;
                        return;
                    }
                    let Ok(data) = s.decoded() else {
                        self.out.cut = true;
                        return;
                    };
                    // Ordinary filtered form decoders may return a prefix at their limit.
                    // Reaching that bound cannot certify a complete form, even at an operator boundary.
                    if !s.filters().is_empty() && data.len() >= self.max_form_bytes {
                        self.out.cut = true;
                        return;
                    }
                    if d.contains_key(b"OC") {
                        // Native attribution currently follows marked content;
                        // matching per-name counts cannot prove form-owned OC
                        // agrees, even when all OCG names are distinct.
                        self.order_issue("Form-owned optional content requires native attribution reconciliation");
                    }
                    let oc = d.get::<Dict<'_>>(b"OC");
                    if let Some(oc) = &oc {
                        self.begin_oc(oc, d.get_ref(b"OC").map(ObjectIdentifier::from));
                        self.hidden_form |= !self.is_visible();
                    }
                    if self.is_visible() {
                        if let Some(g) = d.get::<Dict<'_>>(b"Group") {
                            self.order_issue("Form transparency group colour space requires compositing reconciliation");
                            if self.out.groups.len() >= MAX_GROUPS {
                                self.out.cut = true;
                                return;
                            }
                            self.out.groups.push((g.get::<bool>(b"I").unwrap_or(false), g.get::<bool>(b"K").unwrap_or(false)));
                        }
                        // (Its own resources, or without any those of what draws it: as the interpreter reads them.)
                        let inner = Resources::from_scope(d.get::<Dict<'_>>(b"Resources"), res.clone());
                        self.save();
                        if d.contains_key(b"Matrix") {
                            if let Some(matrix) = d.get::<[f64; 6]>(b"Matrix") {
                                self.concat(matrix);
                            } else if let Some(state) = self.states.last_mut() {
                                state.matrix = None;
                            }
                        }
                        if let Some(state) = self.states.last_mut() {
                            state.clip_context = true;
                        }
                        self.walk(data.as_ref(), &inner, depth + 1);
                        self.restore();
                        if self.out.cut {
                            return;
                        }
                    }
                    if oc.is_some() {
                        self.visible.pop();
                        self.layers.pop();
                    }
                }
                _ => {}
            }
            if let Some((operands, stroke)) = colour {
                let gray = self.state().spaces[usize::from(stroke)] == PrintColorSpace::DeviceGray;
                if let Err(reason) = colour_endpoints_preserved(data.get(start..iter.committed()).unwrap_or_default(), &operands, gray) {
                    self.order_issue(reason);
                }
            }
        }
    }
}

fn colour_operands(op: &TypedInstruction<'_, '_>) -> Option<(Vec<f64>, bool)> {
    let values = match op {
        TypedInstruction::NonStrokeColor(c) => c.0.iter().take(33).map(|n| n.as_f64()).collect(),
        TypedInstruction::StrokeColor(c) => c.0.iter().take(33).map(|n| n.as_f64()).collect(),
        TypedInstruction::NonStrokeColorNamed(c) => c.0.iter().take(33).map(|n| n.as_f64()).collect(),
        TypedInstruction::StrokeColorNamed(c) => c.0.iter().take(33).map(|n| n.as_f64()).collect(),
        TypedInstruction::NonStrokeColorDeviceGray(c) => vec![c.0.as_f64()],
        TypedInstruction::StrokeColorDeviceGray(c) => vec![c.0.as_f64()],
        TypedInstruction::NonStrokeColorDeviceRgb(c) => vec![c.0.as_f64(), c.1.as_f64(), c.2.as_f64()],
        TypedInstruction::StrokeColorDeviceRgb(c) => vec![c.0.as_f64(), c.1.as_f64(), c.2.as_f64()],
        TypedInstruction::NonStrokeColorCmyk(c) => vec![c.0.as_f64(), c.1.as_f64(), c.2.as_f64(), c.3.as_f64()],
        TypedInstruction::StrokeColorCmyk(c) => vec![c.0.as_f64(), c.1.as_f64(), c.2.as_f64(), c.3.as_f64()],
        _ => return None,
    };
    let stroke = matches!(
        op,
        TypedInstruction::StrokeColor(_)
            | TypedInstruction::StrokeColorNamed(_)
            | TypedInstruction::StrokeColorDeviceGray(_)
            | TypedInstruction::StrokeColorDeviceRgb(_)
            | TypedInstruction::StrokeColorCmyk(_)
    );
    Some((values, stroke))
}

/// Raw decimal endpoint classification is compared with the actual typed
/// operands consumed by the interpreter, rather than another numeric parser.
fn colour_endpoints_preserved(raw: &[u8], operands: &[f64], gray: bool) -> Result<(), &'static str> {
    const SYNTAX: &str = "Colour operands require raw/decoded syntax reconciliation";
    const DOMAIN: &str = "Colour operand domain is unsupported by unit-interval precision reconciliation";
    const PRECISION: &str = "Colour component rounding changes zero/full-tone support; raw precision reconciliation required";
    if operands.len() > 32 {
        return Err("Colour operand count exceeds precision reconciliation budget");
    }
    let whitespace = |b: u8| matches!(b, 0 | 9 | 10 | 12 | 13 | 32);
    let delimiter = |b: u8| whitespace(b) || matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%');
    let mut input = raw;
    let mut index = 0;
    let mut issue = None;
    while let Some(&first) = input.first() {
        if whitespace(first) {
            input = input.get(1..).ok_or(SYNTAX)?;
            continue;
        }
        if first == b'%' {
            let end = input.iter().position(|b| matches!(b, b'\r' | b'\n')).unwrap_or(input.len());
            input = input.get(end..).ok_or(SYNTAX)?;
            continue;
        }
        if first == b'/' {
            input = input.get(1..).ok_or(SYNTAX)?;
            let end = input.iter().position(|b| delimiter(*b)).unwrap_or(input.len());
            input = input.get(end..).ok_or(SYNTAX)?;
            continue;
        }
        if delimiter(first) {
            return Err(SYNTAX);
        }
        let end = input.iter().position(|b| delimiter(*b)).unwrap_or(input.len());
        let token = input.get(..end).ok_or(SYNTAX)?;
        input = input.get(end..).ok_or(SYNTAX)?;
        if !first.is_ascii_digit() && !matches!(first, b'+' | b'-' | b'.') {
            continue;
        }
        let text = std::str::from_utf8(token).map_err(|_| SYNTAX)?;
        let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
        if !unsigned.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            || !unsigned.bytes().any(|b| b.is_ascii_digit())
            || unsigned.bytes().filter(|b| *b == b'.').count() > 1
        {
            return Err(SYNTAX);
        }
        let zero = unsigned.bytes().all(|b| b == b'0' || b == b'.');
        let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let integer = integer.trim_start_matches('0');
        let fractional_zero = fraction.bytes().all(|b| b == b'0');
        let one = !text.starts_with('-') && integer == "1" && fractional_zero;
        let decoded = *operands.get(index).ok_or(SYNTAX)?;
        index += 1;
        if (text.starts_with('-') && !zero)
            || (!integer.is_empty() && (integer != "1" || !fractional_zero))
            || !decoded.is_finite()
            || !(0.0..=1.0).contains(&decoded)
        {
            issue.get_or_insert(DOMAIN);
            continue;
        }
        let stored = decoded as f32;
        // DeviceGray is retained as black tone: the importer performs this
        // inversion in f32. Its endpoint loss matters as much as operand loss.
        let inverted = 1.0_f32 - stored;
        if (stored == 0.0) != zero || (stored == 1.0) != one || (gray && ((inverted == 0.0) != one || (inverted == 1.0) != zero)) {
            issue.get_or_insert(PRECISION);
        }
    }
    if index != operands.len() {
        return Err(SYNTAX);
    }
    issue.map_or(Ok(()), Err)
}

/// The fonts of a document by cache key: base font name, and whether the file carries the glyphs
/// ([`embeds_glyphs`]).
pub(crate) type FontNames = HashMap<u128, (String, bool)>;

/// Whether font `f` carries its own glyphs: a font program in its descriptor (a composite font's in
/// its descendant's), or a Type 3 font's glyph procedures. A font that doesn't is drawn with a
/// stand-in, whose glyphs say nothing about which installed face the document means.
fn embeds_glyphs(f: &Dict<'_>) -> bool {
    if f.get::<Name<'_>>(b"Subtype").is_some_and(|s| s.as_ref() == b"Type3") {
        return true;
    }
    // A font program is a stream, so always an indirect object.
    let program = |d: &Dict<'_>| {
        d.get::<Dict<'_>>(b"FontDescriptor").is_some_and(|fd| [&b"FontFile"[..], b"FontFile2", b"FontFile3"].iter().any(|k| fd.get_ref(*k).is_some()))
    };
    program(f) || f.get::<Array<'_>>(b"DescendantFonts").and_then(|a| a.iter::<Dict<'_>>().next()).is_some_and(|d| program(&d))
}

/// Walk `page` as the interpreter will: `all_on` when [`all_on`] turned every group on. Fonts
/// found are noted in `fonts` (their cache key → base font name, and whether the file carries its
/// glyphs).
pub(crate) fn scan_page(page: &Page<'_>, ocgs: &mut Ocgs, all_on: bool, fonts: &mut FontNames, max_form_bytes: usize) -> Result<Scan, PdfError> {
    let scan = walk_page(page, ocgs, all_on, fonts, max_form_bytes, None).out;
    if scan.cut { Err(incomplete_content()) } else { Ok(scan) }
}

fn walk_page<'w, 'p>(
    page: &Page<'p>,
    ocgs: &'w mut Ocgs,
    all_on: bool,
    fonts: &'w mut FontNames,
    max_form_bytes: usize,
    sample_cost: Option<usize>,
) -> Walker<'w, 'p> {
    let off = if all_on { HashSet::new() } else { ocgs.off.clone() };
    let mut w = Walker {
        ocgs,
        off,
        xref: page.xref(),
        fonts,
        visible: vec![],
        layers: vec![],
        marked_floor: 0,
        ops: 0,
        max_ops: max_ops(),
        max_form_bytes,
        out: Scan {
            print_order: sample_cost.map(|sample_cost| crate::print_order::PageOrder { sample_cost, ..Default::default() }),
            ..Default::default()
        },
        hidden_form: false,
        states: vec![State { matrix: Some(vectorcraft_geom::Affine::IDENTITY), ..Default::default() }],
        text: TextOverprint::None,
        text_inks: [RunInk::None; 2],
        ink_index: HashMap::new(),
        inks_cut: false,
        print_sources: HashMap::new(),
    };
    w.page(page);
    if !w.layers.is_empty() {
        w.order_issue("Unbalanced marked content cannot establish print order");
    }
    w.inks_cut |= !w.out.text_inks.set(w.out.paints.len(), w.text_inks.map(RunInk::ink));
    if w.inks_cut {
        w.out.inks.clear();
    }
    w.out.texts.push(w.text);
    w
}

/// Does `page` draw a form XObject that is off by its own optional content (not a marked-content
/// sequence)? Its art couldn't be told apart by group once every group is on.
pub(crate) fn hides_forms(page: &Page<'_>, ocgs: &mut Ocgs, max_form_bytes: usize) -> Result<bool, PdfError> {
    let mut fonts = HashMap::new();
    let walk = walk_page(page, ocgs, false, &mut fonts, max_form_bytes, None);
    if walk.out.cut { Err(incomplete_content()) } else { Ok(walk.hidden_form) }
}

/// `bytes` with an update appended that drops the catalog's `/OCProperties`, so every group
/// draws; `None` when the file's structure can't take one.
pub(crate) fn all_on(bytes: &[u8], pdf: &Pdf) -> Option<Vec<u8>> {
    let root = pdf.xref().root_id();
    let catalog = pdf.xref().get::<Dict<'_>>(root)?;
    let body = catalog.data().strip_suffix(b">>")?;
    let prev = last_xref(bytes)?;
    let trailer = trailer_at(bytes, prev)?;
    let inner = trailer.strip_prefix(b"<<")?.strip_suffix(b">>")?;
    let mut out = bytes.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let at = out.len();
    out.extend(format!("{} {} obj\n", root.obj_number, root.gen_number).bytes());
    out.extend_from_slice(body);
    // A later key wins: the groups' configuration is gone, so the interpreter draws them all.
    out.extend_from_slice(b" /OCProperties null >>\nendobj\n");
    let xref = out.len();
    out.extend(format!("xref\n{} 1\n{at:010} {:05} n \ntrailer\n<<", root.obj_number, root.gen_number).bytes());
    out.extend_from_slice(inner);
    out.extend(format!(" /Prev {prev} >>\nstartxref\n{xref}\n%%EOF\n").bytes());
    Some(out)
}

/// The offset the file's last `startxref` points to.
fn last_xref(bytes: &[u8]) -> Option<usize> {
    let at = bytes.windows(9).rposition(|w| w == b"startxref")?;
    let rest = bytes.get(at + 9..)?;
    let digits: String = rest.iter().skip_while(|c| c.is_ascii_whitespace()).take_while(|c| c.is_ascii_digit()).map(|&c| c as char).collect();
    digits.parse().ok().filter(|&p: &usize| p < bytes.len())
}

/// The trailer dictionary of the cross-reference section at `at` (a table's `trailer`, or a
/// cross-reference stream's dictionary), as written.
fn trailer_at(bytes: &[u8], at: usize) -> Option<&[u8]> {
    let rest = bytes.get(at..)?;
    let from = if rest.trim_ascii_start().starts_with(b"xref") { rest.windows(7).position(|w| w == b"trailer")? } else { 0 };
    let open = from + rest.get(from..)?.windows(2).position(|w| w == b"<<")?;
    let d = Dict::from_bytes(rest.get(open..)?)?;
    Some(d.data())
}

/// Actual content-stream ordering, before editable layer aggregation.
pub(crate) fn scan_print_order(
    page: &Page<'_>,
    ocgs: &mut Ocgs,
    max_form_bytes: usize,
    sample_cost: usize,
) -> Result<crate::print_order::PageOrder, PdfError> {
    let mut fonts = HashMap::new();
    let scan = walk_page(page, ocgs, true, &mut fonts, max_form_bytes, Some(sample_cost)).out;
    if scan.cut {
        return Err(incomplete_content());
    }
    scan.print_order.ok_or_else(incomplete_content)
}

#[cfg(test)]
mod colour_precision_tests {
    use super::{TypedIter, colour_endpoints_preserved, colour_operands};
    fn check(raw: &str, gray: bool) -> Result<(), &'static str> {
        let mut iter = TypedIter::new(raw.as_bytes());
        let op = iter.next().ok_or("Colour operands require raw/decoded syntax reconciliation")?;
        let (operands, _) = colour_operands(&op).ok_or("Colour operands require raw/decoded syntax reconciliation")?;
        colour_endpoints_preserved(raw.as_bytes(), &operands, gray)
    }
    #[test]
    fn literal_colour_endpoints_use_the_actual_typed_operands() {
        for raw in [
            "0 .5 1 .25 k",
            "+1 -0 0 0 k",
            "1% ignored 0.999999999\r\n 0 0 0 k",
            "0.000000000000000000000000000001 0 0 0 k",
            "0\0.5 1 rg",
            "0.0000 01.000 0 0 k",
            ".5/123 scn",
        ] {
            assert!(check(raw, false).is_ok(), "{raw}");
        }
        for raw in [
            "0.999999999 g".to_string(),
            format!("0.{}1 0 0 0 k", "0".repeat(49)),
            format!("0.{}1 scn", "0".repeat(400)),
            "0.999999999999999999999999999999 scn".into(),
            ". g".into(),
            "1-5 g".into(),
            "0.5 1 0 0 0 k".into(),
        ] {
            assert!(check(&raw, false).is_err(), "{raw}");
        }
        // The raw midpoint literal is interior; use both actual possible typed
        // f64 neighbours to prove that classification follows the decoder value.
        let midpoint = 1.0_f64 - 2.0_f64.powi(-25);
        let below = f64::from_bits(midpoint.to_bits() - 1);
        let raw = b"0.9999999701976776123046875 g";
        assert!(colour_endpoints_preserved(raw, &[midpoint], false).is_err());
        assert!(colour_endpoints_preserved(raw, &[below], false).is_ok());
    }
    #[test]
    fn gray_inversion_and_unsupported_domains_have_explicit_refusals() {
        for raw in ["0.00000001 g", "0.00000001 G", "0.00000001 sc", "0.00000001 SCN", "0.999999999 g"] {
            assert!(check(raw, true).unwrap_err().contains("rounding"), "{raw}");
        }
        for raw in ["1. g", "+1 g", "-0 g", ".5 g", "0 G"] {
            assert!(check(raw, true).is_ok(), "{raw}");
        }
        for raw in ["1.5 0 0 0 k", "-0.5 g", "1.000000000000000000000000000001 g"] {
            assert!(check(raw, false).unwrap_err().contains("domain"), "{raw}");
        }
        assert!(check("0.5 1 0 0 0 k", false).unwrap_err().contains("syntax"));
    }
}
