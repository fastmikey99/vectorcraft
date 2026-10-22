//! PDF colours in their own models: DeviceCMYK and CMYK ICC colours stay CMYK, DeviceGray is
//! Gray, DeviceRGB retains its original components, and Separation and DeviceN inks become spot swatches at a tint (for solid paints and
//! shading stops alike). Everything else is RGB, as the interpreter converts it.
//!
//! hayro-interpret keeps a colour's space and components private: their `Debug` form is the only
//! public view, so [`Colors`] reads that (`tests_import_color.rs` pins the format). Ink names
//! aren't in it at all: they come from the file's Separation and DeviceN arrays, found by their
//! tint transform. Where spaces of inks with other names have the same tint transform, the names
//! that the page's walk ([`crate::import_scan`]) finds for each path and run of text pick one.

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Debug, Write as _};
use std::sync::Arc;

use craft_pdf::interpret::Function;
use craft_pdf::interpret::color::ColorSpace;
use craft_pdf::syntax::Pdf;
use craft_pdf::syntax::object::array::FlexArrayIter;
use craft_pdf::syntax::object::{Array, Dict, MaybeRef, Name, Object, Stream};
use craft_pdf::syntax::reader::{Reader, ReaderExt};
use smallvec::SmallVec;
use vectorcraft_color::cms::{self, RgbTransform, WorkingRgb};
use vectorcraft_color::swatch::REGISTRATION;
use vectorcraft_color::{Color, Paint, Swatch};

use crate::import_scan::Rgb;

/// The longest `Debug` text read: a colour whose space is longer (a big sampled tint transform)
/// imports as RGB.
const DEBUG_LIMIT: usize = 64 * 1024;
/// The deepest nesting searched for colour spaces, and the most ink spaces kept.
const MAX_DEPTH: u32 = 32;
const MAX_SPACES: usize = 4096;

/// DeviceN (and Separation) colorants that are process plates, in CMYK channel order.
const PROCESS: [&str; 4] = ["Cyan", "Magenta", "Yellow", "Black"];

/// A colour in its own model; an ink links to its spot swatch at a tint.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Native {
    pub color: Color,
    pub link: Option<(String, f32)>,
}

impl Native {
    fn plain(color: Color) -> Self {
        Self { color, link: None }
    }

    pub fn paint(self) -> Paint {
        match self.link {
            Some((name, tint)) => Paint::Solid { color: self.color, swatch: Some(name), tint },
            None => Paint::solid(self.color),
        }
    }
}

/// What a Separation or DeviceN colorant is.
#[derive(Clone, Copy, Debug)]
enum Colorant {
    /// A spot ink (index into `Colors::inks`).
    Ink(usize),
    /// A process plate (CMYK channel).
    Process(usize),
    /// Every plate (Registration).
    All,
    /// Paints nothing.
    None,
    /// An ink whose colour can't be told (its alternate space isn't one we model).
    Unknown,
}

/// What a channel of an image in a Separation or DeviceN space prints on ([`Colors::image_channels`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ImageChannel {
    /// A process plate (CMYK channel).
    Process(usize),
    /// The plate of a spot swatch (its name).
    Spot(String),
    /// Every plate (Registration).
    All,
    /// Nothing.
    None,
}

/// A Separation or DeviceN colour space of the file.
struct InkSpace {
    /// `Debug` text of its tint transform: how a colour's space is matched to it.
    key: String,
    device_n: bool,
    names: Vec<String>,
    colorants: Vec<Colorant>,
}

struct Ink {
    /// The colorant name in the file.
    source: String,
    /// The swatch name (the source name, made unique against the document's swatches).
    name: String,
    /// The colour at 100 %.
    color: Color,
    used: bool,
}

/// How a colour space's components map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Space {
    Gray,
    Rgb,
    /// An `ICCBased` RGB space whose numbers are taken as they are (see `Colors::uncalibrated_rgb`).
    IccRgb,
    /// RGB in a profile, converted into the RGB working space (index into `Colors::transforms`).
    Profile(usize),
    Cmyk,
    /// A Separation or DeviceN space (index into `Colors::spaces`).
    Inks(usize),
    /// Anything else: RGB through the interpreter.
    Other,
}

/// The alternate space of an ink, for its colour at 100 %.
#[derive(Clone, Copy)]
enum Alt {
    Gray,
    Rgb,
    Cmyk,
    Lab,
}

/// The import's colour mapping, with the inks found and the colour models used.
pub(crate) struct Colors<'p> {
    pdf: &'p Pdf,
    /// Swatch names the document already has.
    taken: Vec<String>,
    scanned: bool,
    spaces: Vec<InkSpace>,
    /// The colorant names of the spaces that have the kind, colorant count and tint transform of a
    /// space of other inks ([`Colors::tied`]; found once).
    tied: Option<HashSet<Vec<String>>>,
    inks: Vec<Ink>,
    /// Process colours painted in CMYK and in RGB (or anything converted to it).
    cmyk: usize,
    rgb: usize,
    /// A DeviceN colour mixed several inks (imported as RGB).
    pub mixed: bool,
    /// RGB colours in an `ICCBased` space keep their numbers instead of going through the profile:
    /// on for an editor's own document (`.ai`), so its art keeps the RGB numbers the file has; off
    /// for PDFs, whose profiles stand.
    pub uncalibrated_rgb: bool,
    /// The RGB working space when it isn't sRGB (`ImportOptions::working_rgb`): RGB colours in a
    /// profile, and the sRGB the interpreter gives for other spaces, are converted into it
    /// ([`Colors::rgb_space`], [`Colors::srgb_in_working`]).
    pub working_rgb: Option<WorkingRgb>,
    /// Conversions from the profiles met into the working space, by profile (`None`: it can't be
    /// read as an RGB profile).
    transforms: Vec<(Arc<[u8]>, Option<RgbTransform>)>,
    /// sRGB into the working space (built when first needed; `None` inside: it can't be).
    srgb: Option<Option<RgbTransform>>,
    /// The file's `ICCBased` RGB profiles (found once, when a colour first needs them).
    file_rgb: Option<FileRgb>,
    /// The page's walk found DeviceRGB colours set (`Scan::device_rgb`).
    page_device_rgb: bool,
    /// The page's walk's profiles met, and their conversions ([`Colors::page_transform`]).
    page_profiles: Vec<(Arc<[u8]>, Option<usize>)>,
}

/// The `ICCBased` RGB profiles of a file, by how the interpreter reads their colours: one whose
/// header names the sRGB device model as DeviceRGB, the others as `ICCBased` (their `Debug`
/// text names no profile).
#[derive(Clone, Copy, Debug)]
struct FileRgb {
    /// Read as DeviceRGB.
    srgb: Group,
    /// Read as `ICCBased`.
    other: Group,
}

/// Profiles of one kind in a file.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Group {
    None,
    /// All with the colours of one profile (index into `Colors::transforms`).
    One(usize),
    /// Several, or ones that can't be read (a Lab profile looks the same, too).
    Several,
}

/// `v`'s `Debug` text, or `None` past [`DEBUG_LIMIT`] (formatting stops there).
fn debug_text(v: &impl Debug) -> Option<String> {
    struct Capped(String);
    impl fmt::Write for Capped {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            if self.0.len() + s.len() > DEBUG_LIMIT {
                return Err(fmt::Error);
            }
            self.0.push_str(s);
            Ok(())
        }
    }
    let mut w = Capped(String::new());
    write!(w, "{v:?}").ok()?;
    Some(w.0)
}

/// A colour's `Debug` text (`Color { color_space: …, components: [..], opacity: .. }`) split into
/// its space's text and its components.
pub(crate) fn split_color(t: &str) -> Option<(&str, Vec<f32>)> {
    let start = t.find("color_space: ")? + "color_space: ".len();
    let at = t.rfind(", components: [")?;
    let space = t.get(start..at)?;
    let rest = t.get(at + ", components: [".len()..)?;
    let list = rest.get(..rest.find(']')?)?;
    let comps = list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::parse::<f32>).collect::<Result<Vec<_>, _>>().ok()?;
    comps.iter().all(|c| c.is_finite()).then_some((space, comps))
}

impl<'p> Colors<'p> {
    pub fn new(pdf: &'p Pdf, taken: Vec<String>) -> Self {
        Self {
            pdf,
            taken,
            scanned: false,
            spaces: vec![],
            tied: None,
            inks: vec![],
            cmyk: 0,
            rgb: 0,
            mixed: false,
            uncalibrated_rgb: false,
            working_rgb: None,
            transforms: vec![],
            srgb: None,
            file_rgb: None,
            page_device_rgb: false,
            page_profiles: vec![],
        }
    }

    /// A solid color in its own model; `None`: use its RGB ([`Colors::srgb_in_working`]). `inks` and
    /// `rgb`: the colorant names and the RGB space of the space the page's walk found it painted
    /// in ([`Colors::space_of`]).
    pub fn solid(&mut self, c: &craft_pdf::interpret::color::Color, inks: Option<&[String]>, rgb: Rgb<'_>) -> Option<Native> {
        let mapped = debug_text(c).and_then(|t| {
            let (space, comps) = split_color(&t)?;
            let space = self.space_of(space, comps.len(), inks, rgb);
            Some((space, comps))
        });
        let (space, comps) = mapped.unwrap_or((Space::Other, vec![]));
        self.count(space);
        self.native(space, &comps)
    }

    /// The space shading colors are in, for [`Colors::native`]; `n`: its component count. `inks`
    /// and `rgb` as for [`Colors::solid`].
    pub fn shading_space(&mut self, cs: &ColorSpace, n: usize, inks: Option<&[String]>, rgb: Rgb<'_>) -> Space {
        let space = debug_text(cs).map_or(Space::Other, |t| self.space_of(&t, n, inks, rgb));
        self.count(space);
        space
    }

    /// Note a colour painted in `space` (the document's colour mode follows the majority).
    fn count(&mut self, space: Space) {
        match space {
            Space::Cmyk => self.cmyk += 1,
            Space::Rgb | Space::IccRgb | Space::Profile(_) | Space::Other => self.rgb += 1,
            Space::Gray | Space::Inks(_) => {}
        }
    }

    /// The document opens in CMYK when more process colours were CMYK than RGB.
    pub fn cmyk_document(&self) -> bool {
        self.cmyk > self.rgb
    }

    /// Spot swatches for the inks used.
    pub fn swatches(&self) -> impl Iterator<Item = Swatch> + '_ {
        self.inks.iter().filter(|i| i.used).map(|i| Swatch { name: i.name.clone(), paint: Paint::solid(i.color), global: true, spot: true })
    }

    /// The space a `ColorSpace(…)` `Debug` text names, with `n` components. `inks`: the colorant
    /// names of the space the page's walk found the color painted in; of the spaces whose tint
    /// transform matches, the one with these names is taken. `rgb`: the RGB space the walk found
    /// it painted in ([`Colors::rgb_space`]).
    pub(crate) fn space_of(&mut self, text: &str, n: usize, inks: Option<&[String]>, rgb: Rgb<'_>) -> Space {
        let inner = text.strip_prefix("ColorSpace(").unwrap_or(text);
        let kind = inner.split(|c: char| !c.is_ascii_alphanumeric()).next().unwrap_or_default();
        match (kind, n) {
            ("DeviceGray" | "ICCBased", 1) => Space::Gray,
            ("DeviceRgb", 3) => self.rgb_space(false, rgb),
            ("ICCBased", 3) => self.rgb_space(true, rgb),
            ("DeviceCmyk" | "ICCBased", 4) => Space::Cmyk,
            ("Separation", 1) | ("DeviceN", _) => {
                self.scan();
                let device_n = kind == "DeviceN";
                let like = |s: &InkSpace| s.device_n == device_n && s.colorants.len() == n && inner.contains(&s.key);
                if let Some(i) = inks.and_then(|names| self.spaces.iter().position(|s| like(s) && s.names == names)) {
                    return Space::Inks(i);
                }
                let mut found = self.spaces.iter().enumerate().filter(|(_, s)| like(s));
                match found.next() {
                    // Spaces of other inks with the same alternate and tint transform look the
                    // same here: without names from the walk, which inks this color uses can't be
                    // told.
                    Some((i, first)) if found.all(|(_, s)| s.names == first.names) => Space::Inks(i),
                    _ => Space::Other,
                }
            }
            _ => Space::Other,
        }
    }

    /// Whether a space of inks `names` has the kind, colorant count and tint transform of a space
    /// of other inks. [`Colors::space_of`] tells such spaces apart only by the names from the
    /// page's walk.
    pub fn tied(&mut self, names: &[String]) -> bool {
        self.scan();
        let spaces = &self.spaces;
        let tied = self.tied.get_or_insert_with(|| {
            let mut first: HashMap<(bool, usize, &str), &Vec<String>> = HashMap::new();
            let mut tied = HashSet::new();
            for s in spaces {
                let f = *first.entry((s.device_n, s.colorants.len(), s.key.as_str())).or_insert(&s.names);
                if *f != s.names {
                    tied.extend([f.clone(), s.names.clone()]);
                }
            }
            tied
        });
        tied.contains(names)
    }

    /// Components `comps` of `space` in their own model; `None`: use their RGB.
    pub fn native(&mut self, space: Space, comps: &[f32]) -> Option<Native> {
        let c = |i: usize| comps.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        match space {
            Space::Gray => Some(Native::plain(Color::gray(1.0 - c(0)))),
            Space::Cmyk => Some(Native::plain(Color::cmyk(c(0), c(1), c(2), c(3)))),
            Space::Rgb | Space::IccRgb => Some(Native::plain(Color::rgb(c(0), c(1), c(2)))),
            Space::Profile(k) => {
                let [r, g, b] = self.transforms.get(k)?.1.as_ref()?.apply([c(0), c(1), c(2)])?;
                Some(Native::plain(Color::rgb(r, g, b)))
            }
            Space::Other => None,
            Space::Inks(i) => self.inks_color(i, comps),
        }
    }

    /// A Separation or DeviceN colour: process plates make a CMYK colour; one ink (or every
    /// plate) at a tint links to its swatch; several inks together can't, and import as RGB.
    fn inks_color(&mut self, space: usize, comps: &[f32]) -> Option<Native> {
        let colorants = &self.spaces.get(space)?.colorants;
        let mut cmyk = [0.0f32; 4];
        let mut ink: Option<(Colorant, f32)> = None;
        let mut first_ink = None;
        let mut several = false;
        for (c, t) in colorants.iter().zip(comps) {
            let t = t.clamp(0.0, 1.0);
            match *c {
                Colorant::Unknown => return None,
                Colorant::None => {}
                Colorant::Process(ch) => {
                    if let Some(v) = cmyk.get_mut(ch) {
                        *v = v.max(t);
                    }
                }
                Colorant::Ink(_) | Colorant::All => {
                    first_ink.get_or_insert(*c);
                    if t > 0.0 {
                        several |= ink.is_some();
                        ink = Some((*c, t));
                    }
                }
            }
        }
        let process = cmyk.iter().any(|v| *v > 0.0);
        let (c, t) = match (ink, first_ink) {
            (Some(_), _) if several || process => {
                self.mixed = true;
                return None;
            }
            (Some(it), _) => it,
            // No ink is painting: 0 % of the first one (a tint of paper), unless the plates are
            // all process ones.
            (None, Some(c)) if !process => (c, 0.0),
            _ => return Some(Native::plain(Color::cmyk(cmyk[0], cmyk[1], cmyk[2], cmyk[3]))),
        };
        match c {
            Colorant::Ink(k) => {
                let ink = self.inks.get_mut(k)?;
                ink.used = true;
                Some(Native { color: ink.color.tinted(t), link: Some((ink.name.clone(), t)) })
            }
            _ => Some(Native { color: Color::cmyk(1.0, 1.0, 1.0, 1.0).tinted(t), link: Some((REGISTRATION.to_string(), t)) }),
        }
    }

    /// How an RGB colour's numbers map: one the interpreter reads in DeviceRGB (`icc` false) or
    /// in an `ICCBased` space (`icc`), painted in `rgb` as the page's walk found it.
    ///
    /// With an RGB working space other than sRGB, RGB in a profile is converted into it (numbers
    /// in the working space's own profile stand): a file's sRGB is not the working space's
    /// numbers. Without the walk's answer, the file's profiles decide where they can: the only
    /// profile of the kind the interpreter reads it as (for DeviceRGB, only on a page whose walk
    /// found no DeviceRGB colours, and not in a reading the walk doesn't follow, where they can't
    /// be ruled out); otherwise DeviceRGB numbers stand and an `ICCBased` colour goes through the
    /// interpreter's sRGB ([`Colors::srgb_in_working`]). DeviceRGB numbers are the
    /// working space's. With sRGB as the working space, the interpreter's sRGB stands, and an
    /// editor's own document (`uncalibrated_rgb`) keeps its RGB numbers, as before.
    fn rgb_space(&mut self, icc: bool, rgb: Rgb<'_>) -> Space {
        if self.uncalibrated_rgb {
            return if icc { Space::IccRgb } else { Space::Rgb };
        }
        if self.working_rgb.is_none() {
            return if icc { Space::Other } else { Space::Rgb };
        }
        match rgb {
            // The reader's sRGB, under a transfer function: from sRGB into the working space.
            Rgb::Srgb => return Space::Other,
            Rgb::Icc(p) => {
                if let Some(k) = self.page_transform(p) {
                    return Space::Profile(k);
                }
            }
            Rgb::NotIcc if !icc => return Space::Rgb,
            _ => {}
        }
        let file = self.file_rgb();
        match (icc, if icc { file.other } else { file.srgb }) {
            (false, Group::None) => Space::Rgb,
            (true, Group::One(k)) => Space::Profile(k),
            (false, Group::One(k)) if !self.page_device_rgb && !matches!(rgb, Rgb::Nested) => Space::Profile(k),
            (true, _) => Space::Other,
            (false, _) => Space::Rgb,
        }
    }

    /// Whether RGB colours are converted into a working space (one other than sRGB, for a file
    /// that isn't an editor's own document): only then does the page's walk note their profiles.
    pub fn converts_rgb(&self) -> bool {
        self.working_rgb.is_some() && !self.uncalibrated_rgb
    }

    /// Whether the RGB spaces page `scan` found for each paint are needed ([`Colors::rgb_space`]):
    /// the page sets a transfer function (the reader gives what it paints as sRGB), or it sets
    /// `ICCBased` RGB spaces and the file's profiles can't say which colours are in which (several
    /// of a kind, or sRGB ones the interpreter reads as DeviceRGB on a page that sets DeviceRGB
    /// colours too).
    pub fn rgb_needs_walk(&mut self, scan: &crate::import_scan::Scan) -> bool {
        if !self.converts_rgb() || (scan.rgb_profiles.is_empty() && !scan.transfer) {
            return false;
        }
        let f = self.file_rgb();
        scan.transfer || f.other == Group::Several || f.srgb == Group::Several || (matches!(f.srgb, Group::One(_)) && scan.device_rgb)
    }

    /// A new page begins: the profiles its walk notes are its own ([`Colors::page_transform`]).
    pub fn begin_page(&mut self, device_rgb: bool) {
        self.page_device_rgb = device_rgb;
        self.page_profiles.clear();
    }

    /// [`Colors::transform`] for a profile of the page's walk (`Scan::rgb_profiles`): found by
    /// its bytes once, then by the shared copy every paint of the page refers to.
    fn page_transform(&mut self, p: &Arc<[u8]>) -> Option<usize> {
        if let Some((_, k)) = self.page_profiles.iter().find(|(q, _)| Arc::ptr_eq(q, p)) {
            return *k;
        }
        let k = self.transform(p);
        self.page_profiles.push((p.clone(), k));
        k
    }

    /// The index in [`Colors::transforms`] of the conversion from RGB profile `p` into the working
    /// space; `None` when it can't be made.
    fn transform(&mut self, p: &[u8]) -> Option<usize> {
        let working = self.working_rgb.as_ref()?;
        let k = match self.transforms.iter().position(|(q, _)| q[..] == p[..]) {
            Some(k) => k,
            None => {
                self.transforms.push((p.into(), RgbTransform::new(p, working)));
                self.transforms.len() - 1
            }
        };
        self.transforms.get(k)?.1.as_ref().map(|_| k)
    }

    /// The file's `ICCBased` RGB profiles, by how the interpreter reads them ([`FileRgb`]; found
    /// once).
    fn file_rgb(&mut self) -> FileRgb {
        if let Some(f) = self.file_rgb {
            return f;
        }
        let mut found: Vec<Vec<u8>> = vec![];
        let mut ok = true;
        for o in self.pdf.objects() {
            icc_rgb_profiles(o, 0, &mut found, &mut ok);
            if !ok {
                break;
            }
        }
        let f = if ok {
            let (srgb, other): (Vec<_>, Vec<_>) = found.into_iter().partition(|p| p.get(52..56) == Some(b"sRGB"));
            FileRgb { srgb: self.group(&srgb), other: self.group(&other) }
        } else {
            FileRgb { srgb: Group::Several, other: Group::Several }
        };
        self.file_rgb = Some(f);
        f
    }

    /// Profiles `ps` as a [`Group`].
    fn group(&mut self, ps: &[Vec<u8>]) -> Group {
        let Some((first, rest)) = ps.split_first() else { return Group::None };
        match self.transform(first) {
            Some(k) if rest.iter().all(|p| cms::same_rgb_profile(p, first)) => Group::One(k),
            _ => Group::Several,
        }
    }

    /// A colour the interpreter gives as sRGB (a space that isn't one of those mapped in their own
    /// model, [`Colors::native`]), in the RGB working space: converted when it isn't sRGB.
    pub fn srgb_in_working(&mut self, [r, g, b]: [f32; 3]) -> Color {
        let Some(working) = &self.working_rgb else { return Color::rgb(r, g, b) };
        let srgb = self.srgb.get_or_insert_with(|| cms::icc_bytes(cms::SRGB).ok().and_then(|p| RgbTransform::new(&p, working)));
        match srgb.as_ref().and_then(|t| t.apply([r, g, b])) {
            Some([r, g, b]) => Color::rgb(r, g, b),
            None => Color::rgb(r, g, b),
        }
    }

    /// Find the file's Separation and DeviceN colour spaces (once, when a colour first needs them).
    fn scan(&mut self) {
        if self.scanned {
            return;
        }
        self.scanned = true;
        let pdf = self.pdf;
        for o in pdf.objects() {
            self.visit(o, 0);
        }
    }

    fn visit(&mut self, o: Object<'_>, depth: u32) {
        if depth > MAX_DEPTH || self.spaces.len() >= MAX_SPACES {
            return;
        }
        match o {
            Object::Array(a) => {
                self.ink_space(&a);
                for item in items(&a).filter_map(direct) {
                    self.visit(item, depth + 1);
                }
            }
            Object::Dict(d) => {
                for v in values(&d) {
                    if let Some(v) = direct(v) {
                        self.visit(v, depth + 1);
                    }
                }
            }
            Object::Stream(s) => self.visit(Object::Dict(s.dict().clone()), depth + 1),
            _ => {}
        }
    }

    /// Record `a` when it is a `[/Separation name alt tint]` or `[/DeviceN [names] alt tint …]` space.
    fn ink_space(&mut self, a: &Array<'_>) -> Option<()> {
        let mut it = a.flex_iter();
        let (device_n, names) = ink_names(&mut it)?;
        let alt = alt_model(&it.next::<Object<'_>>()?);
        let tint = Function::new(&it.next::<Object<'_>>()?)?;
        let key = debug_text(&tint)?;
        if names.is_empty() || self.spaces.iter().any(|s| s.key == key && s.device_n == device_n && s.names == names) {
            return None;
        }
        let colorants = names
            .iter()
            .enumerate()
            .map(|(i, name)| match name.as_str() {
                "None" => Colorant::None,
                "All" => Colorant::All,
                n => match PROCESS.iter().position(|p| *p == n) {
                    Some(ch) => Colorant::Process(ch),
                    None => {
                        // The ink alone at 100 %, through the tint transform.
                        let input: SmallVec<[f32; 4]> = (0..names.len()).map(|j| if i == j { 1.0 } else { 0.0 }).collect();
                        match alt.zip(tint.eval(input)).and_then(|(alt, v)| alt_color(alt, &v)) {
                            Some(color) => Colorant::Ink(self.ink(n, color)),
                            None => Colorant::Unknown,
                        }
                    }
                },
            })
            .collect();
        self.spaces.push(InkSpace { key, device_n, names, colorants });
        Some(())
    }

    /// The plate each channel of image colour space `a` (`[/Separation name alt tint]` or
    /// `[/DeviceN [names] alt tint …]`) prints on, spot inks as their swatches (the swatches of
    /// paints in the same inks: [`Colors::ink`]), which the import then adds. `Err` (why, a warning)
    /// when one is a spot ink whose colour can't be told.
    pub fn image_channels(&mut self, a: &Array<'_>) -> Result<Vec<ImageChannel>, &'static str> {
        const UNKNOWN: &str = "images in spot inks whose colour couldn't be read were imported in RGB, separated into process inks";
        let mut it = a.flex_iter();
        let (_, names) = ink_names(&mut it).ok_or(UNKNOWN)?;
        let alt = it.next::<Object<'_>>().as_ref().and_then(alt_model);
        let tint = it.next::<Object<'_>>().as_ref().and_then(Function::new);
        let mut channels = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            channels.push(match name.as_str() {
                "None" => ImageChannel::None,
                "All" => ImageChannel::All,
                n => match PROCESS.iter().position(|p| *p == n) {
                    Some(ch) => ImageChannel::Process(ch),
                    None => {
                        // The ink alone at 100 %, through the tint transform, as for paints.
                        let input: SmallVec<[f32; 4]> = (0..names.len()).map(|j| if i == j { 1.0 } else { 0.0 }).collect();
                        let color = alt.zip(tint.as_ref()).and_then(|(alt, f)| alt_color(alt, &f.eval(input)?)).ok_or(UNKNOWN)?;
                        let k = self.ink(n, color);
                        let ink = self.inks.get_mut(k).ok_or(UNKNOWN)?;
                        ink.used = true;
                        ImageChannel::Spot(ink.name.clone())
                    }
                },
            });
        }
        Ok(channels)
    }

    /// The ink named `source` (the first definition wins), its swatch name unique in the document.
    fn ink(&mut self, source: &str, color: Color) -> usize {
        if let Some(i) = self.inks.iter().position(|k| k.source == source) {
            return i;
        }
        let mut name = source.to_string();
        let mut n = 2;
        while self.taken.contains(&name) {
            name = format!("{source} {n}");
            n += 1;
        }
        self.inks.push(Ink { source: source.to_string(), name, color, used: false });
        self.inks.len() - 1
    }
}

/// The colorant names of a `[/Separation name …]` or `[/DeviceN [names] …]` color space, read from
/// `it` (the items of its array), and whether it is a DeviceN one; `None` for other spaces.
pub(crate) fn ink_names(it: &mut FlexArrayIter<'_>) -> Option<(bool, Vec<String>)> {
    let device_n = match it.next::<Name<'_>>()?.as_str() {
        "Separation" => false,
        "DeviceN" => true,
        _ => return None,
    };
    let names = if device_n {
        it.next::<Array<'_>>()?.iter::<Name<'_>>().map(|n| n.as_str().to_string()).collect()
    } else {
        vec![it.next::<Name<'_>>()?.as_str().to_string()]
    };
    Some((device_n, names))
}

/// An object written in place (references are visited as objects of their own).
/// Can every item of `a` be read? Not in a damaged file, and hayro's own iterator over the items
/// unwraps each read and panics there.
///
/// The items are read without the array's context. That reading fails wherever the array's own
/// would (a string it can't decrypt is kept as it is either way; a stream whose `/Length` it can't
/// follow falls back to finding `endstream`, as the array's reading does when its length is wrong).
pub(crate) fn readable(a: &Array<'_>) -> bool {
    let mut r = Reader::new(a.data());
    std::iter::from_fn(|| {
        r.skip_white_spaces_and_comments();
        (!r.at_end()).then(|| r.read_without_context::<MaybeRef<Object<'_>>>().is_some())
    })
    .all(|read| read)
}

/// The items of `a`, or none when one of them can't be read ([`readable`]). A file with such an
/// array is refused before it is imported ([`crate::pages`]), so that none never hides what the
/// interpreter would still read of it (its leading items).
pub(crate) fn items<'a>(a: &Array<'a>) -> impl Iterator<Item = MaybeRef<Object<'a>>> + use<'a> {
    readable(a).then(|| a.raw_iter()).into_iter().flatten()
}

pub(crate) fn direct(m: MaybeRef<Object<'_>>) -> Option<Object<'_>> {
    match m {
        MaybeRef::NotRef(o) => Some(o),
        MaybeRef::Ref(_) => None,
    }
}

/// The values of `d` that the reader can parse, sorted by key as `Dict::entries` sorts them.
pub(crate) fn values<'a>(d: &Dict<'a>) -> impl Iterator<Item = MaybeRef<Object<'a>>> {
    let mut keys: Vec<Name<'a>> = d.keys().collect();
    keys.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
    keys.into_iter().filter_map(|k| d.get_raw::<Object<'a>>(k.as_ref()))
}

fn alt_model(o: &Object<'_>) -> Option<Alt> {
    match o {
        Object::Name(n) => match n.as_str() {
            "DeviceGray" | "G" => Some(Alt::Gray),
            "DeviceRGB" | "RGB" => Some(Alt::Rgb),
            "DeviceCMYK" | "CMYK" => Some(Alt::Cmyk),
            _ => None,
        },
        Object::Array(a) => {
            let mut it = a.flex_iter();
            match it.next::<Name<'_>>()?.as_str() {
                "ICCBased" => {
                    let profile = it.next::<Stream<'_>>()?;
                    match profile.dict().get::<u8>(b"N")? {
                        1 => Some(Alt::Gray),
                        3 if lab_profile(&profile) => Some(Alt::Lab),
                        3 => Some(Alt::Rgb),
                        4 => Some(Alt::Cmyk),
                        _ => None,
                    }
                }
                "Lab" => Some(Alt::Lab),
                "CalRGB" => Some(Alt::Rgb),
                "CalGray" => Some(Alt::Gray),
                "CalCMYK" => Some(Alt::Cmyk),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The most different `ICCBased` RGB profiles [`Colors::file_rgb`] takes.
const MAX_ICC_PROFILES: usize = 64;
/// The largest RGB profile read (decoded bytes; an RGB profile, even one with lookup tables, is
/// far smaller), and the most bytes the profiles of a file or a page take together.
pub(crate) const MAX_ICC_BYTES: usize = 4 << 20;
pub(crate) const MAX_ICC_TOTAL: usize = 16 << 20;

/// What the profile of a 3-component `ICCBased` space is ([`icc_rgb`]).
pub(crate) enum IccRgb {
    /// Not a 3-component space.
    NotRgb,
    /// An RGB profile (decoded).
    Rgb(Vec<u8>),
    /// A Lab profile (whose colours look the same as RGB ones to the import), or one that doesn't
    /// decode.
    Unreadable,
}

/// The profile of the `ICCBased` space whose profile stream is `profile`; one larger than
/// [`MAX_ICC_BYTES`] decoded isn't read.
pub(crate) fn icc_rgb(profile: &Stream<'_>) -> IccRgb {
    if profile.dict().get::<u8>(b"N") != Some(3) {
        return IccRgb::NotRgb;
    }
    if lab_profile(profile) {
        return IccRgb::Unreadable;
    }
    // A decoder may give a prefix at the limit: one byte past it says the profile is larger.
    match profile.decoded_within(MAX_ICC_BYTES + 1) {
        Ok(bytes) if bytes.len() <= MAX_ICC_BYTES => IccRgb::Rgb(bytes.into_owned()),
        _ => IccRgb::Unreadable,
    }
}

/// Collect the profiles of the 3-component `ICCBased` spaces in `o` (distinct, decoded) into
/// `found`; `ok` turns false at one that can't be told apart: a Lab profile, one that doesn't
/// decode, or past [`MAX_ICC_PROFILES`] or [`MAX_ICC_TOTAL`].
fn icc_rgb_profiles(o: Object<'_>, depth: u32, found: &mut Vec<Vec<u8>>, ok: &mut bool) {
    if depth > MAX_DEPTH || !*ok {
        return;
    }
    match o {
        Object::Array(a) => {
            let mut it = a.flex_iter();
            if it.next::<Name<'_>>().is_some_and(|n| n.as_str() == "ICCBased") {
                if let Some(profile) = it.next::<Stream<'_>>() {
                    match icc_rgb(&profile) {
                        IccRgb::NotRgb => {}
                        IccRgb::Rgb(bytes) => {
                            if !found.iter().any(|f| f[..] == bytes[..]) {
                                found.push(bytes);
                                *ok = found.len() <= MAX_ICC_PROFILES && found.iter().map(Vec::len).sum::<usize>() <= MAX_ICC_TOTAL;
                            }
                        }
                        IccRgb::Unreadable => *ok = false,
                    }
                }
                return;
            }
            for item in items(&a).filter_map(direct) {
                icc_rgb_profiles(item, depth + 1, found, ok);
            }
        }
        Object::Dict(d) => {
            for v in values(&d) {
                if let Some(v) = direct(v) {
                    icc_rgb_profiles(v, depth + 1, found, ok);
                }
            }
        }
        Object::Stream(s) => icc_rgb_profiles(Object::Dict(s.dict().clone()), depth + 1, found, ok),
        _ => {}
    }
}

/// Is the ICC profile of an `ICCBased` space a Lab one (its alternate space `Lab`, else its header's
/// data colour space `Lab `)? Spot colours defined in Lab come with such a profile as their
/// alternate space (#1032), and their tint transforms give Lab values.
fn lab_profile(profile: &Stream<'_>) -> bool {
    use std::io::Read;
    if let Some(Object::Array(a)) = profile.dict().get::<Object<'_>>(b"Alternate") {
        return a.flex_iter().next::<Name<'_>>().is_some_and(|n| n.as_str() == "Lab");
    }
    // The header's first 20 bytes, without decoding the rest of the profile.
    let raw = profile.raw_data();
    let mut head = vec![];
    match profile.filters().as_slice() {
        [] => head.extend(raw.iter().take(20)),
        [craft_pdf::syntax::Filter::FlateDecode] => {
            let _ = flate2::read::ZlibDecoder::new(&*raw).take(20).read_to_end(&mut head); // a damaged profile reads as not Lab
        }
        _ => return false,
    }
    head.get(16..20) == Some(b"Lab ")
}

/// An alternate-space colour (the tint transform's output) as a VectorCraft colour.
fn alt_color(alt: Alt, v: &[f32]) -> Option<Color> {
    let c = |i: usize| v.get(i).copied().filter(|x| x.is_finite());
    let u = |i: usize| c(i).map(|x| x.clamp(0.0, 1.0));
    Some(match alt {
        Alt::Gray => Color::gray(1.0 - u(0)?),
        Alt::Rgb => Color::rgb(u(0)?, u(1)?, u(2)?),
        Alt::Cmyk => Color::cmyk(u(0)?, u(1)?, u(2)?, u(3)?),
        Alt::Lab => Color::lab(c(0)?.clamp(0.0, 100.0), c(1)?.clamp(-128.0, 127.0), c(2)?.clamp(-128.0, 127.0)),
    })
}
