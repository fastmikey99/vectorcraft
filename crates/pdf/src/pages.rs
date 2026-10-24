//! The pages of a PDF to import: opening with a password, the pages picked, the box each page
//! is cropped to ([`CropTo`]) and [`info`].

use std::collections::{HashMap, HashSet};

use craft_pdf::syntax::object::{Array, Dict, MaybeRef, Name, Object, ObjectIdentifier, Stream, dict_or_stream};
use craft_pdf::syntax::page::Page;
use craft_pdf::syntax::xref::XRef;
use craft_pdf::syntax::{DecryptionError, LoadPdfError, Pdf};
use kurbo::{Affine, Rect};
use vectorcraft_doc::setup::MAX_BLEED;

use crate::import_color::{direct, items, readable, values};
use crate::{Choice, CropTo, ImportOptions, PdfError};

/// What `info` tells about one page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageInfo {
    /// The page's size in points as viewers show it (its crop box, rotated, times the `UserUnit`
    /// the PDF reader applies to the page).
    pub width: f64,
    pub height: f64,
    /// The page's rotation in degrees (0, 90, 180 or 270).
    pub rotation: u16,
    /// Every box but Bounding, in PDF user space (y up), clipped to the media box, times the same
    /// `UserUnit` (in points, as the size). A missing bleed, trim or art box is the crop box.
    pub boxes: Vec<(CropTo, Rect)>,
}

/// The pages of a PDF ([`info`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PdfInfo {
    pub pages: Vec<PageInfo>,
}

/// Is this a PostScript file (plain, or an EPS with a binary header) rather than a PDF?
pub fn is_postscript(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%!PS") || bytes.starts_with(&[0xC5, 0xD0, 0xD3, 0xC6])
}

/// The application's stricter refusal policy for functions and colour spaces built on one
/// another (a function stitching functions together, an indexed space on an alternate).
/// The shared interpreter also has function depth and node budgets; this preflight rejects
/// cyclic or excessively linked objects before interpretation. Files usually have two or three.
const MAX_LINKS: usize = 16;

/// The application's calculator-procedure depth policy, layered over the shared reader's limits.
const MAX_PROCEDURES: usize = 64;

/// What the function or colour space `o` is built on: the functions a stitching function joins;
/// the base, alternate and tint of a colour space; the alternate of a colour profile. Nothing for
/// any other object.
fn links<'a>(o: &Object<'a>, xref: &'a XRef) -> Vec<MaybeRef<Object<'a>>> {
    const SPACES: [&[u8]; 6] = [b"Indexed", b"I", b"Separation", b"DeviceN", b"Pattern", b"ICCBased"];
    if let Object::Array(a) = o {
        let mut items = items(a);
        let kind = match items.next() {
            Some(MaybeRef::NotRef(Object::Name(n))) => Some(n),
            Some(MaybeRef::Ref(r)) => xref.get::<Name<'a>>(r.into()),
            _ => None,
        };
        return if kind.is_some_and(|n| SPACES.contains(&n.as_ref())) { items.collect() } else { vec![] };
    }
    let Some((d, _)) = dict_or_stream(o) else { return vec![] };
    let functions = d.get::<Array<'a>>(b"Functions").map(|a| items(&a).collect::<Vec<_>>()).unwrap_or_default();
    functions.into_iter().chain(d.get_raw::<Object<'a>>(b"Alternate")).collect()
}

/// How many of its kind the function or colour space `o` is built on, one on the next (0 for any
/// other object); `None` past `room`, or when it is built on itself. `known`: the objects looked
/// at so far (`None` while one is being looked at, too). One `None` refuses the file, so that an
/// object is `None` only for the room there was where it was first met doesn't matter.
fn built_on<'a>(o: MaybeRef<Object<'a>>, xref: &'a XRef, known: &mut HashMap<ObjectIdentifier, Option<usize>>, room: usize) -> Option<usize> {
    let (id, o) = match o {
        MaybeRef::Ref(r) => {
            let id = ObjectIdentifier::from(r);
            if let Some(found) = known.get(&id) {
                return *found;
            }
            let Some(o) = xref.get::<Object<'a>>(id) else { return Some(0) };
            known.insert(id, None);
            (Some(id), o)
        }
        MaybeRef::NotRef(o) => (None, o),
    };
    let links = links(&o, xref);
    let most = match room.checked_sub(1) {
        _ if links.is_empty() => Some(0),
        Some(room) => links.into_iter().try_fold(0, |most, on| built_on(on, xref, known, room).map(|n| most.max(n + 1))),
        None => None,
    };
    // (What it is built on may be known already, and deeper than there is room for here.)
    let most = most.filter(|n| *n <= room);
    if let Some(id) = id {
        known.insert(id, most);
    }
    most
}

/// Is `o` a calculator function whose procedures nest deeper than [`MAX_PROCEDURES`]?
fn procedures_too_deep(o: &Object<'_>) -> bool {
    let Object::Stream(s) = o else { return false };
    if s.dict().get::<u8>(b"FunctionType") != Some(4) {
        return false;
    }
    let Ok(program) = s.decoded() else { return false };
    let mut depth = 0usize;
    program.iter().any(|c| {
        match c {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        depth > MAX_PROCEDURES
    })
}

/// The error of a file with a stream the reader would fail on ([`crate::streams`]).
const STREAM_NOTE: &str = "a stream in it would decode to more than 256 MB (the content of a page or form: 512 MB, \
                           at most 32 times its stored size), or its decode parameters can't be applied";

/// The error of a file with functions or color spaces built on one another past [`MAX_LINKS`]
/// (or on themselves), or a calculator function nested past [`MAX_PROCEDURES`].
const NESTED_NOTE: &str = "its functions or colour spaces are built on one another without end";

/// The error of a file with an array whose items can't all be read (a damaged file).
const UNREADABLE_NOTE: &str = "an array in it is damaged and can't be read";

/// Why `pdf` is refused, if it is: a stream the reader would fail on ([`STREAM_NOTE`]), or
/// functions, color spaces or calculator procedures nested too deep ([`NESTED_NOTE`]). Every
/// object is looked at, and every array, dictionary and stream written inside one. Objects
/// reached exclusively through an unshared catalog or page-tree leaf's `PieceInfo` are exempt:
/// drawing never decodes them, including during colour discovery.
/// Shared streams keep their checks, including when a private dictionary is itself shared.
fn hostile(pdf: &Pdf) -> Option<&'static str> {
    let xref = pdf.xref();
    let refs = references(pdf.objects().into_iter());
    let objects = scanned_objects_with_refs(pdf, &refs, u32::MAX);
    let content = content_streams(pdf, &refs);
    let mut known = HashMap::new();
    for o in objects {
        // A stream's own checks come first: a calculator function is decoded below.
        if matches!(&o, Object::Stream(s) if crate::streams::unsafe_stream(s, content.contains(&s.obj_id()))) {
            return Some(STREAM_NOTE);
        }
        if procedures_too_deep(&o) || built_on(MaybeRef::NotRef(o.clone()), xref, &mut known, MAX_LINKS).is_none() {
            return Some(NESTED_NOTE);
        }
        // An array with an item that can't be read can't be looked at whole, and the
        // interpreter would still read its leading items: the file is refused.
        if matches!(&o, Object::Array(a) if !readable(a)) {
            return Some(UNREADABLE_NOTE);
        }
    }
    None
}

/// The streams `pdf` uses as content and as nothing else. Content is found where the interpreter
/// finds it, from the pages of the page tree: a page's `Contents` (a stream or an array of them);
/// in the resources of a page (its own or inherited), a form, a tiling pattern, a Type 3 font or
/// an appearance stream, the forms of `XObject`, the tiling patterns of `Pattern`, a Type 3
/// font's `CharProcs` and the group (`G`) of a graphics state's soft mask; and the appearance
/// streams (`AP`: `N`, `R` and `D`, each a stream or a dictionary of them) of a page's
/// annotations. A stream the file also refers to from anywhere else (an Indexed colour table, a
/// function, an unrelated dictionary's `Contents`) is not content here: it gets the ordinary
/// limit ([`crate::streams`]).
fn content_streams(pdf: &Pdf, refs: &HashMap<ObjectIdentifier, usize>) -> HashSet<ObjectIdentifier> {
    let mut uses = Uses {
        xref: pdf.xref(),
        seen: HashSet::new(),
        inherited: HashSet::new(),
        roles: HashMap::new(),
        content: HashMap::new(),
        path: vec![],
        depth: 0,
    };
    for page in pdf.pages().iter() {
        uses.page(page.raw());
    }
    // Content only, and reached only through objects the file uses for nothing else: an object
    // also referred to from anywhere else (`/XObject 7 0 R /Shading 7 0 R`) passes no content on.
    let only_here = |id: &ObjectIdentifier| uses.roles.get(id) == refs.get(id);
    uses.content.iter().filter(|(id, within)| only_here(id) && within.iter().all(only_here)).map(|(id, _)| *id).collect()
}

/// Count every reference in these objects and their direct children, without following it.
/// Both content grants and private-data exemptions must account for every incoming reference.
fn references<'a>(objects: impl Iterator<Item = Object<'a>>) -> HashMap<ObjectIdentifier, usize> {
    let mut refs = HashMap::new();
    let mut pending: Vec<_> = objects.collect();
    while let Some(o) = pending.pop() {
        let held: Vec<MaybeRef<Object<'_>>> = match &o {
            Object::Array(a) => items(a).collect(),
            _ => dict_or_stream(&o).into_iter().flat_map(|(d, _)| values(d).collect::<Vec<_>>()).collect(),
        };
        for h in held {
            match h {
                MaybeRef::Ref(r) => *refs.entry(ObjectIdentifier::from(r)).or_default() += 1,
                MaybeRef::NotRef(o) => pending.push(o),
            }
        }
    }
    refs
}

/// Objects reached only through an unshared catalog or page's `PieceInfo` (ISO 32000-1
/// §14.5). A form or a reader root also used elsewhere cannot grant an exemption.
/// https://developer.adobe.com/document-services/docs/assets/35e4369068f86065372c18787171a17e/PDF_ISO_32000-1.pdf
fn private_objects(pdf: &Pdf, refs: &HashMap<ObjectIdentifier, usize>) -> HashSet<(usize, usize)> {
    let xref = pdf.xref();
    let catalog = xref.get::<Dict<'_>>(xref.root_id());
    let mut roots = vec![];
    if refs.get(&xref.root_id()).copied().unwrap_or(0) == 0 {
        roots.extend(catalog.clone());
    }
    // Confirm membership through the real page tree, rather than the reader's repair fallback.
    // Each leaf must have exactly its one Kids reference; resources or any other use revoke it.
    let pages: HashSet<_> = pdf.pages().iter().filter_map(|p| p.raw().obj_id()).collect();
    let mut pending: Vec<_> = catalog
        .and_then(|d| d.get::<Dict<'_>>(b"Pages"))
        .filter(|d| d.get::<Name<'_>>(b"Type").is_some_and(|t| t.as_ref() == b"Pages"))
        .into_iter()
        .collect();
    let mut seen = HashSet::new();
    let mut work = 1usize;
    while let Some(node) = pending.pop() {
        if node.obj_id().is_some_and(|id| !seen.insert(id)) {
            continue;
        }
        if node.get::<Name<'_>>(b"Type").is_some_and(|t| t.as_ref() == b"Pages") {
            if let Some(kids) = node.get::<Array<'_>>(b"Kids") {
                for kid in items(&kids) {
                    work += 1;
                    if work > MAX_PAGE_TREE {
                        // An incomplete ownership walk grants nothing.
                        return HashSet::new();
                    }
                    if let MaybeRef::Ref(r) = kid {
                        pending.extend(xref.get::<Dict<'_>>(r.into()));
                    }
                }
            }
        } else if node.get::<Name<'_>>(b"Type").is_some_and(|t| t.as_ref() == b"Page")
            && node.obj_id().is_some_and(|id| pages.contains(&id) && refs.get(&id) == Some(&1))
        {
            roots.push(node);
        }
    }
    let public: HashSet<_> = std::iter::once(xref.root_id()).chain(pages).collect();
    let mut granted = HashMap::<ObjectIdentifier, usize>::new();
    let mut skipped = HashSet::new();
    for root in roots {
        let Some(piece) = root.get::<Dict<'_>>(b"PieceInfo") else { continue };
        let children = match root.get_raw::<Object<'_>>(b"PieceInfo") {
            Some(MaybeRef::Ref(r)) => HashMap::from([(r.into(), 1)]),
            Some(MaybeRef::NotRef(o)) => {
                skipped.insert(identity(piece.data()));
                references(std::iter::once(o))
            }
            None => HashMap::new(),
        };
        for (id, n) in children {
            *granted.entry(id).or_default() += n;
        }
    }
    // First grant the whole reachable graph, counting each edge once, including cycles and
    // repeated references. Then revoke any object with an outside reference, and propagate
    // that revocation to its descendants. This is linear even for a shared or cyclic graph.
    let mut pending: Vec<_> = granted.keys().copied().collect();
    let mut edges = HashMap::new();
    while let Some(id) = pending.pop() {
        if edges.contains_key(&id) {
            continue;
        }
        let children = references(xref.get::<Object<'_>>(id).into_iter());
        for (child, n) in &children {
            *granted.entry(*child).or_default() += n;
            if !edges.contains_key(child) {
                pending.push(*child);
            }
        }
        edges.insert(id, children);
    }
    let mut private: HashSet<_> = edges.keys().copied().collect();
    pending.extend(private.iter().filter(|id| public.contains(id) || granted.get(id) != refs.get(id)).copied());
    while let Some(id) = pending.pop() {
        if !private.remove(&id) {
            continue;
        }
        for (child, n) in edges.get(&id).into_iter().flatten() {
            if let Some(count) = granted.get_mut(child) {
                *count = count.saturating_sub(*n);
                if Some(&*count) != refs.get(child) {
                    pending.push(*child);
                }
            }
        }
    }
    for id in private {
        if let Some(o) = xref.get::<Object<'_>>(id)
            && let Some(data) = object_data(&o)
            // Reader structural streams always retain their checks.
            && !matches!(&o, Object::Stream(s) if s.dict().get::<Name<'_>>(b"Type")
                .is_some_and(|t| matches!(t.as_ref(), b"ObjStm" | b"XRef")))
        {
            skipped.insert(identity(data));
        }
    }
    skipped
}

// Identify the actual parsed container, including arrays and dictionaries inside object streams.
// Equal bytes in a different object do not establish ownership. The PDF outlives the walk.
fn identity(data: &[u8]) -> (usize, usize) {
    (data.as_ptr() as usize, data.len())
}

fn object_data<'a>(o: &Object<'a>) -> Option<&'a [u8]> {
    match o {
        Object::Array(a) => Some(a.data()),
        _ => dict_or_stream(o).map(|(d, _)| d.data()),
    }
}

/// All containers that retain normal screening, with their direct children. Both preflight and
/// colour discovery use this walk so private tint functions and profiles are never decoded.
pub(crate) fn scanned_objects(pdf: &Pdf, max_depth: u32) -> impl Iterator<Item = Object<'_>> {
    let refs = references(pdf.objects().into_iter());
    scanned_objects_with_refs(pdf, &refs, max_depth)
}

fn scanned_objects_with_refs<'a>(
    pdf: &'a Pdf,
    refs: &HashMap<ObjectIdentifier, usize>,
    max_depth: u32,
) -> impl Iterator<Item = Object<'a>> + use<'a> {
    let skipped = private_objects(pdf, refs);
    let mut pending: Vec<_> = pdf.objects().into_iter().map(|o| (o, 0u32)).collect();
    pending.reverse();
    std::iter::from_fn(move || {
        while let Some((o, depth)) = pending.pop() {
            if depth > max_depth {
                continue;
            }
            if object_data(&o).is_some_and(|data| skipped.contains(&identity(data))) {
                continue;
            }
            let children: Vec<_> = match &o {
                Object::Array(a) => items(a).filter_map(direct).collect(),
                _ => dict_or_stream(&o).into_iter().flat_map(|(d, _)| values(d)).filter_map(direct).collect(),
            };
            // Colour discovery historically counts a stream and its dictionary separately.
            let child_depth = depth.saturating_add(if matches!(o, Object::Stream(_)) { 2 } else { 1 });
            pending.extend(children.into_iter().rev().map(|child| (child, child_depth)));
            return Some(o);
        }
        None
    })
}

/// How deep [`content_streams`] follows content inside content (forms in forms, glyphs in
/// glyphs): the interpreter's limit on nested forms. Content past it gets the ordinary limit.
const MAX_CONTENT_NESTING: usize = 50;

/// How many page tree nodes above a page [`content_streams`] looks through for the resources it
/// inherits: the reader's page tree work budget, so any page it reads has its resources found.
const MAX_PAGE_TREE: usize = 100_000;

/// The walk of [`content_streams`]: each place that holds content is looked at once.
struct Uses<'a> {
    xref: &'a XRef,
    /// The objects looked at already.
    seen: HashSet<ObjectIdentifier>,
    /// The page tree nodes whose resources were walked for the pages that inherit them.
    inherited: HashSet<ObjectIdentifier>,
    /// The references to each object from where the walk found it.
    roles: HashMap<ObjectIdentifier, usize>,
    /// Each content stream, and the objects the walk went through to find it (anywhere).
    content: HashMap<ObjectIdentifier, HashSet<ObjectIdentifier>>,
    /// The objects the walk is inside, below the page.
    path: Vec<ObjectIdentifier>,
    /// How many resource dictionaries the walk is inside.
    depth: usize,
}

impl<'a> Uses<'a> {
    /// The object `o` is or refers to, the first time it is met (`None` after that); a
    /// reference is counted each time.
    fn first(&mut self, o: MaybeRef<Object<'a>>) -> Option<Object<'a>> {
        match o {
            MaybeRef::Ref(r) => {
                let id = ObjectIdentifier::from(r);
                *self.roles.entry(id).or_default() += 1;
                if self.seen.insert(id) { self.xref.get::<Object<'a>>(id) } else { None }
            }
            MaybeRef::NotRef(o) => Some(o),
        }
    }

    /// The dictionary `o` is or refers to, the first time it is met, and the object it is.
    fn dict(&mut self, o: Option<MaybeRef<Object<'a>>>) -> Option<(Dict<'a>, Option<ObjectIdentifier>)> {
        let o = o?;
        let id = o.as_obj_ref().map(ObjectIdentifier::from);
        match self.first(o)? {
            Object::Dict(d) => Some((d, id)),
            _ => None,
        }
    }

    /// `f`, inside the object `id` (when it is one).
    fn within(&mut self, id: Option<ObjectIdentifier>, f: impl FnOnce(&mut Self)) {
        self.path.extend(id);
        f(self);
        if id.is_some() {
            self.path.pop();
        }
    }

    /// The stream `o` refers to, used as content from here when `kind` accepts its dictionary;
    /// its resources are walked the first time. `false` when `o` isn't a stream.
    fn content(&mut self, o: &MaybeRef<Object<'a>>, kind: fn(&Dict<'_>) -> bool) -> bool {
        // (A stream is always an indirect object.)
        let MaybeRef::Ref(r) = o else { return false };
        let id = ObjectIdentifier::from(*r);
        let Some(Object::Stream(s)) = self.xref.get::<Object<'a>>(id) else { return false };
        *self.roles.entry(id).or_default() += 1;
        if kind(s.dict()) {
            self.content.entry(id).or_default().extend(self.path.iter().copied());
            if self.seen.insert(id) {
                self.within(Some(id), |u| u.resources(s.dict().get_raw::<Object<'a>>(b"Resources")));
            }
        }
        true
    }

    /// The stream `o` refers to as content, or the streams of the array (or, `in_dict`, the
    /// dictionary of appearance states) it is or refers to.
    fn streams(&mut self, o: Option<MaybeRef<Object<'a>>>, in_dict: bool) {
        let Some(o) = o else { return };
        if self.content(&o, any) {
            return;
        }
        let id = o.as_obj_ref().map(ObjectIdentifier::from);
        match self.first(o) {
            Some(Object::Array(a)) if !in_dict => self.within(id, |u| items(&a).for_each(|i| _ = u.content(&i, any))),
            Some(Object::Dict(d)) if in_dict => self.within(id, |u| values(&d).for_each(|v| _ = u.content(&v, any))),
            _ => {}
        }
    }

    fn page(&mut self, page: &Dict<'a>) {
        if page.obj_id().is_some_and(|id| !self.seen.insert(id)) {
            return;
        }
        self.streams(page.get_raw::<Object<'a>>(b"Contents"), false);
        // Its own resources, or those of the nearest node above it (walked once for all its pages).
        let mut node = page.clone();
        let mut above = HashSet::new();
        for _ in 0..MAX_PAGE_TREE {
            if let Some(res) = node.get_raw::<Object<'a>>(b"Resources") {
                if node.obj_id() == page.obj_id() || node.obj_id().is_some_and(|id| self.inherited.insert(id)) {
                    self.resources(Some(res));
                }
                break;
            }
            let Some(parent) = node.get::<Dict<'a>>(b"Parent") else { break };
            if parent.obj_id().is_some_and(|id| !above.insert(id)) {
                break;
            }
            node = parent;
        }
        let annots_id = page.get_ref(b"Annots").map(ObjectIdentifier::from);
        let Some(Object::Array(annots)) = page.get_raw::<Object<'a>>(b"Annots").and_then(|a| self.first(a)) else { return };
        self.within(annots_id, |u| {
            for annot in items(&annots) {
                let Some((annot, annot_id)) = u.dict(Some(annot)) else { continue };
                u.within(annot_id, |u| {
                    let Some((ap, ap_id)) = u.dict(annot.get_raw::<Object<'a>>(b"AP")) else { return };
                    u.within(ap_id, |u| {
                        for key in [&b"N"[..], b"R", b"D"] {
                            let Some(a) = ap.get_raw::<Object<'a>>(key) else { continue };
                            if !u.content(&a, any) {
                                u.streams(Some(a), true);
                            }
                        }
                    });
                });
            }
        });
    }

    fn resources(&mut self, res: Option<MaybeRef<Object<'a>>>) {
        if self.depth >= MAX_CONTENT_NESTING {
            return;
        }
        let Some((res, id)) = self.dict(res) else { return };
        self.depth += 1;
        self.within(id, |u| u.resources_in(&res));
        self.depth -= 1;
    }

    fn resources_in(&mut self, res: &Dict<'a>) {
        if let Some((x, id)) = self.dict(res.get_raw::<Object<'a>>(b"XObject")) {
            self.within(id, |u| values(&x).for_each(|v| _ = u.content(&v, form)));
        }
        if let Some((p, id)) = self.dict(res.get_raw::<Object<'a>>(b"Pattern")) {
            self.within(id, |u| values(&p).for_each(|v| _ = u.content(&v, |d| d.get::<u8>(b"PatternType") == Some(1))));
        }
        if let Some((fonts, id)) = self.dict(res.get_raw::<Object<'a>>(b"Font")) {
            self.within(id, |u| {
                for f in values(&fonts) {
                    let Some((f, font_id)) = u.dict(Some(f)) else { continue };
                    if f.get::<Name<'_>>(b"Subtype").is_some_and(|s| s.as_ref() == b"Type3") {
                        u.within(font_id, |u| {
                            if let Some((procs, procs_id)) = u.dict(f.get_raw::<Object<'a>>(b"CharProcs")) {
                                u.within(procs_id, |u| values(&procs).for_each(|v| _ = u.content(&v, any)));
                            }
                            u.resources(f.get_raw::<Object<'a>>(b"Resources"));
                        });
                    }
                }
            });
        }
        if let Some((states, id)) = self.dict(res.get_raw::<Object<'a>>(b"ExtGState")) {
            self.within(id, |u| {
                for gs in values(&states) {
                    let Some((gs, gs_id)) = u.dict(Some(gs)) else { continue };
                    u.within(gs_id, |u| {
                        if let Some((mask, mask_id)) = u.dict(gs.get_raw::<Object<'a>>(b"SMask"))
                            && let Some(g) = mask.get_raw::<Object<'a>>(b"G")
                        {
                            u.within(mask_id, |u| _ = u.content(&g, any));
                        }
                    });
                }
            });
        }
    }
}

/// Any stream: a page's contents, a glyph's procedure, an appearance stream or a soft mask's
/// group is drawn as content whatever its dictionary says.
fn any(_: &Dict<'_>) -> bool {
    true
}

/// A form XObject.
fn form(d: &Dict<'_>) -> bool {
    d.get::<Name<'_>>(b"Subtype").is_some_and(|s| s.as_ref() == b"Form")
}

/// Read a PDF, decrypting it with `password` (none: the empty user password). The permissions
/// password opens a file too.
///
/// What a hostile file nests or loops is the shared reader's to refuse (it reads arrays and
/// dictionaries 64 deep and no deeper, wherever they are: the file's text, an object stream, the
/// content of a page, form, pattern or glyph; and follows a page tree or a chain of object streams
/// once). What it reads beyond that is left out, as an object it can't read. The application also
/// rejects overly linked functions and colour spaces, oversized streams and unsupported
/// predictors before interpretation ([`hostile`], [`crate::streams`]), retaining its stricter
/// refusal policy alongside the shared backend's bounded decoding and construction.
pub(crate) fn open(bytes: &[u8], password: Option<&str>) -> Result<Pdf, PdfError> {
    open_with_options(bytes, password, &open_options())
}

/// Preserve the application's existing accepted file sizes. Ordinary stream decoding still
/// uses the shared reader's immutable per-document limits, and its input copy is fallible.
pub(crate) fn open_options() -> craft_pdf::OpenOptions {
    craft_pdf::OpenOptions { max_input_bytes: usize::MAX, ..Default::default() }
}

pub(crate) fn open_with_options(bytes: &[u8], password: Option<&str>, options: &craft_pdf::OpenOptions) -> Result<Pdf, PdfError> {
    if is_postscript(bytes) {
        return Err(PdfError::PostScript);
    }
    if crate::streams::unsafe_file_streams(bytes) {
        return Err(PdfError::Parse(STREAM_NOTE.into()));
    }
    let password = password.unwrap_or_default();
    let read = |pw: &str| craft_pdf::open_slice_with_options(bytes, Some(pw), options);
    read(password)
        .or_else(|e| match e {
            // The reader takes only the open password of RC4 and 128-bit AES files: the
            // permissions password gives it.
            craft_pdf::OpenError::Load(LoadPdfError::Decryption(DecryptionError::PasswordProtected)) if !password.is_empty() => {
                crate::encrypt::user_password(bytes, password).map_or(Err(e), |user| read(&user))
            }
            e => Err(e),
        })
        .map_err(|e| match e {
            craft_pdf::OpenError::Load(LoadPdfError::Decryption(DecryptionError::PasswordProtected)) if password.is_empty() => {
                PdfError::NeedsPassword
            }
            craft_pdf::OpenError::Load(LoadPdfError::Decryption(DecryptionError::PasswordProtected)) => PdfError::WrongPassword,
            e => PdfError::Parse(e.to_string()),
        })
        // Twice: looking at every object makes the reader repair a damaged table of objects (once
        // only), and the objects are other ones after that.
        .and_then(|pdf| match (0..2).find_map(|_| hostile(&pdf)) {
            Some(why) => Err(PdfError::Parse(why.into())),
            None => Ok(pdf),
        })
}

/// The pages and their boxes. An encrypted PDF needs its `password`
/// ([`PdfError::NeedsPassword`] / [`PdfError::WrongPassword`]).
pub fn info(bytes: &[u8], password: Option<&str>) -> Result<PdfInfo, PdfError> {
    let pdf = open(bytes, password)?;
    let pages = pdf
        .pages()
        .iter()
        .map(|page| {
            let (w, h) = page.render_dimensions();
            let rotation = match page.rotation() {
                craft_pdf::syntax::page::Rotation::None => 0,
                craft_pdf::syntax::page::Rotation::Horizontal => 90,
                craft_pdf::syntax::page::Rotation::Flipped => 180,
                craft_pdf::syntax::page::Rotation::FlippedHorizontal => 270,
            };
            let unit = f64::from(page.user_unit());
            let boxes = CropTo::ALL.iter().filter(|c| **c != CropTo::Bounding).map(|c| (*c, page_box(page, *c).scale_from_origin(unit))).collect();
            PageInfo { width: w as f64, height: h as f64, rotation, boxes }
        })
        .collect();
    Ok(PdfInfo { pages })
}

/// The 0-based pages `opts` pick out of `count`.
pub(crate) fn picked(opts: &ImportOptions, count: usize) -> Result<Vec<usize>, PdfError> {
    let mut v = match &opts.pages {
        Some(p) => {
            if let Some(bad) = p.iter().find(|i| **i >= count) {
                return Err(PdfError::BadPage(bad.saturating_add(1), count));
            }
            p.clone()
        }
        None => (0..count).collect(),
    };
    if let Some(m) = opts.max_pages {
        v.truncate(m);
    }
    if v.is_empty() {
        return Err(PdfError::NoPages);
    }
    Ok(v)
}

fn rect(r: craft_pdf::syntax::object::Rect) -> Rect {
    Rect::new(r.x0, r.y0, r.x1, r.y1).abs()
}

/// Box `which` of `page` in PDF user space, clipped to the media box (Bounding: the crop box,
/// which the art's bounds replace after import). A missing or empty box is the crop box.
pub(crate) fn page_box(page: &Page<'_>, which: CropTo) -> Rect {
    let exact = |key: &[u8], inherit: bool| {
        let mut dict = page.raw().clone();
        for _ in 0..32 {
            if let Some(v) = dict.get::<[f64; 4]>(key) {
                let r = Rect::new(v[0], v[1], v[2], v[3]).abs();
                return (r.is_finite() && r.area() > 0.0).then_some(r);
            }
            if !inherit {
                break;
            }
            let Some(parent) = dict.get::<Dict<'_>>(b"Parent") else { break };
            dict = parent;
        }
        None
    };
    let media = exact(b"MediaBox", true).unwrap_or_else(|| rect(page.media_box()));
    let crop = exact(b"CropBox", true).unwrap_or(media).intersect(media);
    let key: &[u8] = match which {
        CropTo::Bounding | CropTo::Crop => return crop,
        CropTo::Media if media.area() > 0.0 => return media,
        CropTo::Media => return crop,
        CropTo::Bleed => b"BleedBox",
        CropTo::Trim => b"TrimBox",
        CropTo::Art => b"ArtBox",
    };
    exact(key, false).map(|r| r.intersect(media)).filter(|r| r.area() > 0.0).unwrap_or(crop)
}

/// Where `page` draws on its artboard: the page transform (PDF user space → y-down page space
/// with the crop box at the origin, rotation applied) and `which` box in that space (a page with
/// no area gets the size viewers give it).
pub(crate) fn frame(page: &Page<'_>, which: CropTo) -> (Affine, Rect) {
    let init = Affine::new(page.initial_transform(true).as_coeffs());
    let b = init.transform_rect_bbox(page_box(page, which));
    let (w, h) = page.render_dimensions();
    (init, if b.is_finite() && b.area() > 0.0 { b } else { Rect::new(0.0, 0.0, w as f64, h as f64) })
}

/// How far `page`'s bleed box reaches past its trim box as the page shows (rotated): the bleed
/// `[top, bottom, left, right]` in points, kept to 0–[`MAX_BLEED`].
pub(crate) fn bleed(page: &Page<'_>) -> [f64; 4] {
    let init = Affine::new(page.initial_transform(true).as_coeffs());
    let (trim, bleed) = (init.transform_rect_bbox(page_box(page, CropTo::Trim)), init.transform_rect_bbox(page_box(page, CropTo::Bleed)));
    [trim.y0 - bleed.y0, bleed.y1 - trim.y1, trim.x0 - bleed.x0, bleed.x1 - trim.x1]
        .map(|b| if b.is_finite() { b.clamp(0.0, MAX_BLEED) } else { 0.0 })
}

/// The `PieceInfo` key such files keep their private data under.
const PRIVATE_DATA_OWNER: &[u8] = b"Illustrator"; // brand-ok: the key the files use

/// Most bytes of an editor's private data read.
const MAX_PRIVATE: usize = 256 << 20;

/// The private data of an Illustrator `.ai` (its first page's `PieceInfo`): the `AIPrivateData`
/// streams in order, joined. A PDF saved with the editing data has its streams under
/// `AIPDFPrivateData1`, `AIPDFPrivateData2`, … instead: those are read when there is no
/// `AIPrivateData1`. `None` for a PDF without either (or with a stream that can't be read).
/// What it holds is the editor's own copy of the art (see `vectorcraft_eps::layered_ai`).
pub fn illustrator_data(bytes: &[u8], password: Option<&str>) -> Option<Vec<u8>> {
    let pdf = open(bytes, password).ok()?;
    let page = pdf.pages().first()?;
    let private = page.raw().get::<Dict<'_>>(b"PieceInfo")?.get::<Dict<'_>>(PRIVATE_DATA_OWNER)?.get::<Dict<'_>>(b"Private")?;
    let key = if private.contains_key(b"AIPrivateData1") { "AIPrivateData" } else { "AIPDFPrivateData" };
    let mut data = vec![];
    for n in 1..=100_000u32 {
        let Some(stream) = private.get::<Stream<'_>>(format!("{key}{n}").as_bytes()) else { break };
        data.extend_from_slice(&inflated(&stream, MAX_PRIVATE - data.len())?);
    }
    (!data.is_empty()).then_some(data)
}

/// Does the PDF's first page carry an editor's copy of the art: a PDF-compatible `.ai`
/// (`AIPrivateData` keys) or a PDF saved with its editing data (`AIPDFPrivateData`, #1345)? Without
/// reading the data. Either's trim boxes are its editor's artboards.
pub fn has_illustrator_data(bytes: &[u8], password: Option<&str>) -> bool {
    open(bytes, password).is_ok_and(|pdf| pdf.pages().first().is_some_and(|p| piece_info_has(p, &[b"AIPrivateData", b"AIPDFPrivateData"])))
}

/// `stream` decoded, up to `most` bytes: the decoder is run here rather than by the PDF reader's
/// `decoded()`, which has no limit, so a stream of zeros can't grow to the size of the memory. Only a
/// stream that is plain or only deflated is read (the private data is no other); else, or when it
/// is larger once decoded, `None`.
fn inflated(stream: &Stream<'_>, most: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    if stream.dict().get::<Dict<'_>>(b"DecodeParms").is_some() {
        return None;
    }
    let raw = stream.raw_data();
    let mut out = vec![];
    let limit = u64::try_from(most).ok()?.checked_add(1)?;
    match stream.filters().len() {
        0 => out.extend_from_slice(&raw),
        1 => {
            flate2::read::ZlibDecoder::new(&*raw).take(limit).read_to_end(&mut out).ok()?;
        }
        _ => return None,
    }
    (out.len() <= most).then_some(out)
}

/// Does `page` carry an editor's private data (`PieceInfo` keys starting with `AIPrivateData`)?
/// A file saved without PDF compatibility keeps its art there and shows a placeholder page.
pub(crate) fn has_private_data(page: &Page<'_>) -> bool {
    piece_info_has(page, &[b"AIPrivateData"])
}

/// Does `page` state where its art is: an art, trim or bleed box (ISO 32000-1 §14.11.2, the
/// extent of the page's meaningful content and of the finished page)? A page written from an
/// artboard has them; the notice a file saved without its PDF part shows instead of its art is
/// the same page whatever the document, with no art to state the place of.
pub(crate) fn states_art(page: &Page<'_>) -> bool {
    [&b"ArtBox"[..], b"TrimBox", b"BleedBox"].iter().any(|k| page.raw().get::<[f32; 4]>(*k).is_some())
}

/// Does `page`'s `PieceInfo` hold a key starting with one of `prefixes`, up to four dictionaries in?
fn piece_info_has(page: &Page<'_>, prefixes: &[&[u8]]) -> bool {
    fn walk(d: &Dict<'_>, prefixes: &[&[u8]], depth: u32) -> bool {
        depth < 4
            && d.keys().any(|k| {
                prefixes.iter().any(|p| k.starts_with(p)) || d.get::<Dict<'_>>(k.as_ref()).is_some_and(|sub| walk(&sub, prefixes, depth + 1))
            })
    }
    page.raw().get::<Dict<'_>>(b"PieceInfo").is_some_and(|d| walk(&d, prefixes, 0))
}
