//! ICC profiles made in code for colour tests (no profile file is committed).

/// sRGB's primaries with linear tone curves (an ICC file).
pub fn linear_srgb() -> Vec<u8> {
    let mut p = moxcms::ColorProfile::new_srgb();
    p.cicp = None;
    for trc in [&mut p.red_trc, &mut p.green_trc, &mut p.blue_trc] {
        *trc = Some(moxcms::ToneReprCurve::Parametric(vec![1.0]));
    }
    p.encode().unwrap()
}

/// An RGB profile (an ICC file) made of lookup tables: sRGB's colours through Lab, its perceptual
/// table out of the profile (A2B0) darkening by `out` and the one into it (B2A0) by `into` (1:
/// the same as the colorimetric ones), for tests of colour conversion by intent.
pub fn rgb_lut_profile(out: f32, into: f32) -> Vec<u8> {
    use moxcms::{
        ColorProfile, DataColorSpace, LutMultidimensionalType, LutStore, LutWarehouse, Matrix3d, ProfileClass, RenderingIntent, ToneReprCurve,
        Vector3d,
    };
    const N: usize = 17;
    let unit = |i: usize| i as f32 / (N - 1) as f32;
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    let a2b = |k: f32| {
        let mut t = Vec::with_capacity(N.pow(3) * 3);
        for i in 0..N.pow(3) {
            let rgb = [2, 1, 0].map(|d| unit(i / N.pow(d) % N) * k);
            let l = vectorcraft_color::cms::lab::srgb_to_lab(rgb);
            t.extend([l.l / 100.0, (l.a + 128.0) / 255.0, (l.b + 128.0) / 255.0].map(q));
        }
        t
    };
    let b2a = |k: f32| {
        let mut t = Vec::with_capacity(N.pow(3) * 3);
        for i in 0..N.pow(3) {
            let [l, a, b] = [2, 1, 0].map(|d| unit(i / N.pow(d) % N));
            let rgb = vectorcraft_color::cms::lab::lab_to_srgb(vectorcraft_color::cms::Lab::new(l * 100.0, a * 255.0 - 128.0, b * 255.0 - 128.0));
            t.extend(rgb.map(|v| q(v / k)));
        }
        t
    };
    let table = |clut: Vec<u16>| {
        let mut grid_points = [0u8; 16];
        grid_points[..3].fill(N as u8);
        LutWarehouse::Multidimensional(LutMultidimensionalType {
            num_input_channels: 3,
            num_output_channels: 3,
            grid_points,
            clut: Some(LutStore::Store16(clut)),
            a_curves: vec![ToneReprCurve::Parametric(vec![1.0]); 3],
            b_curves: vec![ToneReprCurve::Parametric(vec![1.0]); 3],
            m_curves: vec![],
            matrix: Matrix3d::IDENTITY,
            bias: Vector3d::default(),
        })
    };
    let mut p = ColorProfile::default();
    p.profile_class = ProfileClass::DisplayDevice;
    p.color_space = DataColorSpace::Rgb;
    p.pcs = DataColorSpace::Lab;
    p.rendering_intent = RenderingIntent::Perceptual;
    p.white_point = moxcms::WHITE_POINT_D50.to_xyzd();
    p.media_white_point = Some(p.white_point);
    for (intent, k, kb) in [(0, out, into), (1, 1.0, 1.0), (2, 1.0, 1.0)] {
        let (fwd, back) = (Some(table(a2b(k))), Some(table(b2a(kb))));
        match intent {
            0 => (p.lut_a_to_b_perceptual, p.lut_b_to_a_perceptual) = (fwd, back),
            1 => (p.lut_a_to_b_colorimetric, p.lut_b_to_a_colorimetric) = (fwd, back),
            _ => (p.lut_a_to_b_saturation, p.lut_b_to_a_saturation) = (fwd, back),
        }
    }
    p.encode().unwrap()
}
