//! ICC profiles through `moxcms` (pure Rust): built-in RGB spaces and user-supplied `.icc` files,
//! and the profiles exports embed ([`encode_builtin`]), written in code from the CMS.

use std::sync::{Arc, Mutex, OnceLock};

use moxcms::{
    ColorProfile, DataColorSpace, Layout, LocalizableString, LutMultidimensionalType, LutStore, LutWarehouse, Matrix3d, ProfileClass, ProfileText,
    RenderingIntent, ToneReprCurve, TransformF32Executor, TransformOptions, Vector3d,
};

use super::{CmsError, Intent, ProfileKind, lab};

fn moxcms_intent(i: Intent) -> RenderingIntent {
    match i {
        Intent::Perceptual => RenderingIntent::Perceptual,
        Intent::RelativeColorimetric => RenderingIntent::RelativeColorimetric,
        Intent::Saturation => RenderingIntent::Saturation,
        Intent::AbsoluteColorimetric => RenderingIntent::AbsoluteColorimetric,
    }
}

const INTENTS: [Intent; 4] = [Intent::Perceptual, Intent::RelativeColorimetric, Intent::Saturation, Intent::AbsoluteColorimetric];

fn intent_index(i: Intent) -> usize {
    INTENTS.iter().position(|x| *x == i).unwrap_or(1)
}

type Xf = Option<Arc<TransformF32Executor>>;

/// A direct transform's cache key: destination name, intent index, black-point compensation.
type DirectKey = (String, usize, bool);

/// A parsed ICC profile plus lazily-built transforms to/from sRGB.
pub struct IccProfile {
    pub name: String,
    pub kind: ProfileKind,
    /// The file it was loaded from (empty for profiles made in code).
    source: Vec<u8>,
    profile: ColorProfile,
    srgb: ColorProfile,
    to_srgb: [OnceLock<Xf>; 4],
    from_srgb: [OnceLock<Xf>; 4],
    /// Direct transforms into other RGB profiles ([`Self::to_rgb_of`]), by destination, intent
    /// and black-point compensation.
    direct: Mutex<Vec<(DirectKey, Option<Arc<Direct>>)>>,
    /// Direct transforms into CMYK profiles ([`Self::to_cmyk_of`]), keyed the same way.
    direct_cmyk: Mutex<Vec<(DirectKey, Option<Arc<DirectCmyk>>)>>,
}

/// A transform into a CMYK profile without passing through sRGB: one step, or with black-point
/// compensation two, through linear ProPhoto RGB (wide enough for any print gamut), where the
/// source black is remapped onto the destination's.
struct DirectCmyk {
    /// Straight into the destination (no compensation).
    one: Option<Arc<TransformF32Executor>>,
    /// Into linear ProPhoto RGB, and from it into the destination (with compensation).
    to_hub: Option<Arc<TransformF32Executor>>,
    from_hub: Option<Arc<TransformF32Executor>>,
    /// The source's and the destination's black, as luminance in the hub.
    black: (f32, f32),
}

/// A transform from one profile into an RGB profile without passing through sRGB: into the
/// destination's primaries with linear tone curves, where black-point compensation is a linear
/// remap, then onto its own curves.
struct Direct {
    to_linear: Arc<TransformF32Executor>,
    /// Linear → the destination's curves; `None` when the destination isn't a matrix profile
    /// (the first transform goes straight into it, without compensation).
    to_dest: Option<Arc<TransformF32Executor>>,
    /// The source black's luminance in the linear destination (0: no compensation).
    black: f32,
}

impl std::fmt::Debug for IccProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IccProfile").field("name", &self.name).field("kind", &self.kind).finish()
    }
}

fn text(t: &Option<ProfileText>) -> Option<String> {
    match t.as_ref()? {
        ProfileText::PlainString(s) => Some(s.clone()),
        ProfileText::Localizable(v) => v.iter().find(|l| l.language == "en").or(v.first()).map(|l| l.value.clone()),
        ProfileText::Description(d) => Some(if d.ascii_string.is_empty() { d.unicode_string.clone() } else { d.ascii_string.clone() }),
    }
    .map(|s| s.trim_matches(char::from(0)).trim().to_string())
    .filter(|s| !s.is_empty())
}

impl IccProfile {
    pub fn from_profile(name: Option<String>, profile: ColorProfile) -> Result<Self, CmsError> {
        let kind = match profile.color_space {
            DataColorSpace::Rgb => ProfileKind::Rgb,
            DataColorSpace::Cmyk => ProfileKind::Cmyk,
            DataColorSpace::Gray => ProfileKind::Gray,
            other => return Err(CmsError::Unsupported(format!("profile colour space {other:?} is not RGB, CMYK or Gray"))),
        };
        let name = name.or_else(|| text(&profile.description)).unwrap_or_else(|| format!("Untitled {kind:?} profile"));
        Ok(Self {
            name,
            kind,
            source: Vec::new(),
            profile,
            srgb: ColorProfile::new_srgb(),
            to_srgb: Default::default(),
            from_srgb: Default::default(),
            direct: Mutex::new(Vec::new()),
            direct_cmyk: Mutex::new(Vec::new()),
        })
    }

    pub fn from_bytes(name: Option<String>, bytes: &[u8]) -> Result<Self, CmsError> {
        let p = ColorProfile::new_from_slice(bytes).map_err(|e| CmsError::BadProfile(format!("{e:?}")))?;
        Ok(Self { source: bytes.to_vec(), ..Self::from_profile(name, p)? })
    }

    /// The profile as an ICC file: the bytes it was loaded from, else encoded.
    pub fn bytes(&self) -> Result<Vec<u8>, CmsError> {
        if self.source.is_empty() { encode(&self.profile) } else { Ok(self.source.clone()) }
    }

    fn layout(&self) -> Layout {
        match self.kind {
            ProfileKind::Cmyk => Layout::Rgba, // 4 channels
            ProfileKind::Gray => Layout::Gray,
            ProfileKind::Rgb => Layout::Rgb,
        }
    }

    fn channels(&self) -> usize {
        match self.kind {
            ProfileKind::Cmyk => 4,
            ProfileKind::Gray => 1,
            ProfileKind::Rgb => 3,
        }
    }

    fn opts(intent: Intent) -> TransformOptions {
        TransformOptions { rendering_intent: moxcms_intent(intent), ..Default::default() }
    }

    fn to_xf(&self, intent: Intent) -> Xf {
        self.to_srgb[intent_index(intent)]
            .get_or_init(|| self.profile.create_transform_f32(self.layout(), &self.srgb, Layout::Rgb, Self::opts(intent)).ok())
            .clone()
    }

    fn inverse_xf(&self, intent: Intent) -> Xf {
        self.from_srgb[intent_index(intent)]
            .get_or_init(|| self.srgb.create_transform_f32(Layout::Rgb, &self.profile, self.layout(), Self::opts(intent)).ok())
            .clone()
    }

    /// Device values (RGB / CMYK / Gray, 0..1) → sRGB.
    pub fn to_srgb(&self, v: &[f32], intent: Intent) -> Option<[f32; 3]> {
        let xf = self.to_xf(intent)?;
        let mut src = v.to_vec();
        src.resize(self.channels(), 0.0);
        let mut dst = [0.0f32; 3];
        xf.transform(&src, &mut dst).ok()?;
        Some(dst.map(|x| x.clamp(0.0, 1.0)))
    }

    /// sRGB → device values.
    pub fn from_srgb(&self, rgb: [f32; 3], intent: Intent) -> Option<Vec<f32>> {
        let xf = self.inverse_xf(intent)?;
        let mut dst = vec![0.0f32; self.channels()];
        xf.transform(&rgb, &mut dst).ok()?;
        Some(dst.into_iter().map(|x| x.clamp(0.0, 1.0)).collect())
    }

    /// Device values (RGB / CMYK / Gray, 0..1) → `dest`'s RGB, converted directly with `intent`:
    /// not through sRGB, so colours outside sRGB keep their place in a wider space. With `bpc`
    /// and relative colorimetric, this profile's black maps onto the destination's black
    /// (black-point compensation, the darkest ink the profile prints becoming RGB 0 rather than a
    /// dark grey). Perceptual tables already map black, so compensation leaves them alone, as
    /// lcms and Ghostscript do. `None` when `dest` isn't RGB or no transform can be built.
    pub fn to_rgb_of(&self, dest: &IccProfile, v: &[f32], intent: Intent, bpc: bool) -> Option<[f32; 3]> {
        let t = self.direct_xf(dest, intent, bpc && intent == Intent::RelativeColorimetric)?;
        let mut src = v.to_vec();
        src.resize(self.channels(), 0.0);
        let mut lin = [0.0f32; 3];
        t.to_linear.transform(&src, &mut lin).ok()?;
        let Some(to_dest) = &t.to_dest else { return Some(lin.map(|x| x.clamp(0.0, 1.0))) };
        if t.black > 0.0 {
            lin = lin.map(|x| (x - t.black) / (1.0 - t.black));
        }
        let lin = lin.map(|x| x.clamp(0.0, 1.0));
        let mut out = [0.0f32; 3];
        to_dest.transform(&lin, &mut out).ok()?;
        Some(out.map(|x| x.clamp(0.0, 1.0)))
    }

    fn direct_xf(&self, dest: &IccProfile, intent: Intent, bpc: bool) -> Option<Arc<Direct>> {
        if dest.kind != ProfileKind::Rgb {
            return None;
        }
        let key = (dest.name.clone(), intent_index(intent), bpc);
        let mut cache = self.direct.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, t)) = cache.iter().find(|(k, _)| *k == key) {
            return t.clone();
        }
        let t = self.build_direct(dest, intent, bpc).map(Arc::new);
        cache.push((key, t.clone()));
        t
    }

    fn build_direct(&self, dest: &IccProfile, intent: Intent, bpc: bool) -> Option<Direct> {
        let d = &dest.profile;
        let matrix = d.red_trc.is_some() && d.green_trc.is_some() && d.blue_trc.is_some();
        if !matrix {
            let xf = self.profile.create_transform_f32(self.layout(), d, Layout::Rgb, Self::opts(intent)).ok()?;
            return Some(Direct { to_linear: xf, to_dest: None, black: 0.0 });
        }
        let mut linear = d.clone();
        for trc in [&mut linear.red_trc, &mut linear.green_trc, &mut linear.blue_trc] {
            *trc = Some(ToneReprCurve::Parametric(vec![1.0]));
        }
        // A coding-independent code point names the transfer curve outright (moxcms's sRGB has
        // one) and would override the linear curves.
        linear.cicp = None;
        let to_linear = self.profile.create_transform_f32(self.layout(), &linear, Layout::Rgb, Self::opts(intent)).ok()?;
        let to_dest = linear.create_transform_f32(Layout::Rgb, d, Layout::Rgb, Self::opts(Intent::RelativeColorimetric)).ok()?;
        // The source black: the darkest colour the profile reproduces, found as lcms does for an
        // output profile (black sent into the profile and read back), made neutral.
        let black = if bpc && self.kind == ProfileKind::Cmyk {
            let inks = self.from_srgb([0.0; 3], Intent::RelativeColorimetric)?;
            let mut lin = [0.0f32; 3];
            to_linear.transform(&inks, &mut lin).ok()?;
            let y = |c: &moxcms::Xyzd| c.y as f32;
            let lum = lin[0] * y(&d.red_colorant) + lin[1] * y(&d.green_colorant) + lin[2] * y(&d.blue_colorant);
            lum.clamp(0.0, 0.5)
        } else {
            0.0
        };
        Some(Direct { to_linear, to_dest: Some(to_dest), black })
    }

    /// Device values (RGB / CMYK, 0..1) → `dest`'s CMYK, converted directly with `intent`: not
    /// through sRGB, so a CMYK colour outside sRGB (CMYK cyan) reaches another CMYK profile
    /// whole. With `bpc` and relative colorimetric, this profile's black maps onto the
    /// destination's (black-point compensation); perceptual tables already map black. `None`
    /// when `dest` isn't CMYK or no transform can be built.
    pub fn to_cmyk_of(&self, dest: &IccProfile, v: &[f32], intent: Intent, bpc: bool) -> Option<[f32; 4]> {
        let t = self.direct_cmyk_xf(dest, intent, bpc && intent == Intent::RelativeColorimetric)?;
        let mut src = v.to_vec();
        src.resize(self.channels(), 0.0);
        let mut out = [0.0f32; 4];
        match (&t.one, &t.to_hub, &t.from_hub) {
            (Some(one), _, _) => one.transform(&src, &mut out).ok()?,
            (None, Some(to), Some(from)) => {
                let mut lin = [0.0f32; 3];
                to.transform(&src, &mut lin).ok()?;
                let (ks, kd) = t.black;
                let lin = lin.map(|x| ((x - ks) / (1.0 - ks) * (1.0 - kd) + kd).clamp(0.0, 1.0));
                from.transform(&lin, &mut out).ok()?;
            }
            _ => return None,
        }
        Some(out.map(|x| x.clamp(0.0, 1.0)))
    }

    fn direct_cmyk_xf(&self, dest: &IccProfile, intent: Intent, bpc: bool) -> Option<Arc<DirectCmyk>> {
        if dest.kind != ProfileKind::Cmyk {
            return None;
        }
        let key = (dest.name.clone(), intent_index(intent), bpc);
        let mut cache = self.direct_cmyk.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, t)) = cache.iter().find(|(k, _)| *k == key) {
            return t.clone();
        }
        let t = self.build_direct_cmyk(dest, intent, bpc).map(Arc::new);
        cache.push((key, t.clone()));
        t
    }

    fn build_direct_cmyk(&self, dest: &IccProfile, intent: Intent, bpc: bool) -> Option<DirectCmyk> {
        // Always through the hub: moxcms's one-step link between two CMYK LUT profiles maps some
        // colours badly under perceptual intent (CMYK cyan lost a quarter of its cyan ink), while
        // each profile's own table into and out of a matrix space is sound.
        let hub = linear_prophoto();
        let opts = Self::opts(intent);
        let to_hub = self.profile.create_transform_f32(self.layout(), &hub, Layout::Rgb, opts).ok()?;
        let from_hub = hub.create_transform_f32(Layout::Rgb, &dest.profile, dest.layout(), opts).ok()?;
        let black = if bpc {
            let dest_to_hub = dest.profile.create_transform_f32(dest.layout(), &hub, Layout::Rgb, opts).ok()?;
            (self.black_in(&hub, &*to_hub)?, dest.black_in(&hub, &*dest_to_hub)?)
        } else {
            (0.0, 0.0)
        };
        Some(DirectCmyk { one: None, to_hub: Some(to_hub), from_hub: Some(from_hub), black })
    }

    /// The darkest colour this profile reproduces, as luminance in linear matrix space `hub`
    /// (reached by `to_hub`): black sent into the profile and read back, as lcms finds an output
    /// profile's black point. An RGB matrix profile's black is 0.
    fn black_in(&self, hub: &ColorProfile, to_hub: &TransformF32Executor) -> Option<f32> {
        if self.kind != ProfileKind::Cmyk {
            return Some(0.0);
        }
        let inks = self.from_srgb([0.0; 3], Intent::RelativeColorimetric)?;
        let mut lin = [0.0f32; 3];
        to_hub.transform(&inks, &mut lin).ok()?;
        let y = |c: &moxcms::Xyzd| c.y as f32;
        Some((lin[0] * y(&hub.red_colorant) + lin[1] * y(&hub.green_colorant) + lin[2] * y(&hub.blue_colorant)).clamp(0.0, 0.5))
    }

    /// Whether transforms can be built for this profile (checked at registration).
    pub fn usable(&self) -> bool {
        self.to_xf(Intent::RelativeColorimetric).is_some() && self.inverse_xf(Intent::RelativeColorimetric).is_some()
    }
}

/// ProPhoto RGB with linear tone curves: a matrix space wide enough to hold any print gamut, used
/// as the hub where black-point compensation remaps luminance.
fn linear_prophoto() -> ColorProfile {
    let mut p = ColorProfile::new_pro_photo_rgb();
    for trc in [&mut p.red_trc, &mut p.green_trc, &mut p.blue_trc] {
        *trc = Some(ToneReprCurve::Parametric(vec![1.0]));
    }
    p.cicp = None;
    p
}

/// The primaries and tone curves of built-in RGB space `name`.
fn rgb_space(name: &str) -> Option<ColorProfile> {
    Some(match name {
        super::WIDE_GAMUT_RGB => ColorProfile::new_adobe_rgb(),
        super::DISPLAY_P3 => ColorProfile::new_display_p3(),
        super::PROPHOTO_RGB => ColorProfile::new_pro_photo_rgb(),
        super::SRGB => ColorProfile::new_srgb(),
        _ => return None,
    })
}

/// Built-in RGB working spaces from `moxcms`'s standard primaries.
pub fn builtin_rgb(name: &str) -> Option<IccProfile> {
    IccProfile::from_profile(Some(name.to_string()), rgb_space(name)?).ok()
}

/// Built-in RGB space `name` as an ICC file, under its name here.
pub(super) fn builtin_rgb_bytes(name: &str) -> Result<Vec<u8>, CmsError> {
    encode(&labelled(rgb_space(name).ok_or_else(|| CmsError::UnknownProfile(name.into()))?, name))
}

/// Encode `p` as an ICC file. The creation date is fixed so exports are reproducible.
pub(super) fn encode(p: &ColorProfile) -> Result<Vec<u8>, CmsError> {
    let mut bytes = p.encode().map_err(|e| CmsError::BadProfile(format!("{e:?}")))?;
    // Header bytes 24..36: year, month, day, hours, minutes, seconds (u16 each).
    if let Some(date) = bytes.get_mut(24..36) {
        date.copy_from_slice(&[2026u16, 1, 1, 0, 0, 0].map(u16::to_be_bytes).concat());
    }
    Ok(bytes)
}

fn text_tag(s: &str) -> Option<ProfileText> {
    Some(ProfileText::Localizable(vec![LocalizableString::new("en".into(), "US".into(), s.into())]))
}

/// Profiles made here carry their name and no copyright claim.
fn labelled(mut p: ColorProfile, name: &str) -> ColorProfile {
    p.description = text_tag(name);
    p.copyright = text_tag("No copyright, use freely");
    p
}

/// Grid points per input channel of the CMYK → Lab table and of the Lab → CMYK table.
const A2B_GRID: usize = 9;
const B2A_GRID: usize = 17;

/// A CMYK output profile sampled from a conversion: `to_lab` (CMYK → media-relative Lab) in a
/// 9⁴ table, `from_lab` (Lab → CMYK, gamut-mapped) in a 17³ table, Lab PCS (ICC v4 encoding).
pub(super) fn cmyk_profile(name: &str, to_lab: impl Fn([f32; 4]) -> lab::Lab, from_lab: impl Fn(lab::Lab) -> [f32; 4]) -> ColorProfile {
    let unit = |i: usize, n: usize| i as f32 / (n - 1) as f32;
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    let mut a2b = Vec::with_capacity(A2B_GRID.pow(4) * 3);
    for i in 0..A2B_GRID.pow(4) {
        let cmyk = [3, 2, 1, 0].map(|d| unit(i / A2B_GRID.pow(d) % A2B_GRID, A2B_GRID));
        let l = to_lab(cmyk);
        a2b.extend([l.l / 100.0, (l.a + 128.0) / 255.0, (l.b + 128.0) / 255.0].map(q));
    }
    let mut b2a = Vec::with_capacity(B2A_GRID.pow(3) * 4);
    for i in 0..B2A_GRID.pow(3) {
        let [l, a, b] = [2, 1, 0].map(|d| unit(i / B2A_GRID.pow(d) % B2A_GRID, B2A_GRID));
        b2a.extend(from_lab(lab::Lab::new(l * 100.0, a * 255.0 - 128.0, b * 255.0 - 128.0)).map(q));
    }
    let identity = |n: usize| vec![ToneReprCurve::Parametric(vec![1.0]); n];
    let table = |inputs: usize, outputs: usize, grid: usize, clut: Vec<u16>| {
        let mut grid_points = [0u8; 16];
        grid_points[..inputs].fill(grid as u8);
        LutWarehouse::Multidimensional(LutMultidimensionalType {
            num_input_channels: inputs as u8,
            num_output_channels: outputs as u8,
            grid_points,
            clut: Some(LutStore::Store16(clut)),
            // A curves sit on the device side, B curves on the PCS side.
            a_curves: identity(4),
            b_curves: identity(3),
            m_curves: vec![],
            matrix: Matrix3d::IDENTITY,
            bias: Vector3d::default(),
        })
    };
    let a2b = table(4, 3, A2B_GRID, a2b);
    let b2a = table(3, 4, B2A_GRID, b2a);
    let mut p = ColorProfile::default();
    p.profile_class = ProfileClass::OutputDevice;
    p.color_space = DataColorSpace::Cmyk;
    p.pcs = DataColorSpace::Lab;
    p.rendering_intent = RenderingIntent::RelativeColorimetric;
    p.white_point = moxcms::WHITE_POINT_D50.to_xyzd();
    p.media_white_point = Some(p.white_point);
    p.lut_a_to_b_perceptual = Some(a2b.clone());
    p.lut_a_to_b_colorimetric = Some(a2b);
    p.lut_b_to_a_perceptual = Some(b2a.clone());
    p.lut_b_to_a_colorimetric = Some(b2a);
    labelled(p, name)
}

/// The grey space of greyscale exports: sRGB's tone curve, D50 white.
pub(super) fn gray_profile(name: &str) -> ColorProfile {
    let mut p = ColorProfile::new_gray_with_gamma(2.2);
    p.gray_trc = ColorProfile::new_srgb().red_trc;
    labelled(p, name)
}
