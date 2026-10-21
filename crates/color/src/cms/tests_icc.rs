use super::*;

fn parse(bytes: &[u8]) -> moxcms::ColorProfile {
    moxcms::ColorProfile::new_from_slice(bytes).unwrap()
}

#[test]
fn builtin_rgb_spaces_encode_under_their_names() {
    for name in BUILTIN_RGB {
        let p = IccProfile::from_bytes(None, &icc_bytes(name).unwrap()).unwrap();
        assert_eq!((p.name.as_str(), p.kind), (name, ProfileKind::Rgb));
    }
    // Legacy names find today's profile.
    assert_eq!(icc_bytes(LEGACY_NAMES[0].0).unwrap(), icc_bytes(WIDE_GAMUT_RGB).unwrap());
    assert!(matches!(icc_bytes("No Such Profile"), Err(CmsError::UnknownProfile(_))));
}

#[test]
fn cmyk_profiles_convert_like_the_cms() {
    for name in BUILTIN_CMYK {
        let bytes = icc_bytes(name).unwrap();
        assert_eq!(parse(&bytes).color_space, moxcms::DataColorSpace::Cmyk);
        let p = IccProfile::from_bytes(None, &bytes).unwrap();
        assert_eq!((p.name.as_str(), p.kind), (name, ProfileKind::Cmyk));
        // The encoded profile converts without black-point compensation, so compare without it.
        let cms = Cms::new(&ColorSettings { cmyk: name.into(), bpc: false, ..Default::default() }).unwrap();
        for cmyk in [[0.0; 4], [1.0, 0.0, 0.0, 0.0], [0.2, 0.7, 0.1, 0.3], [0.0, 0.0, 0.0, 1.0], [0.5, 0.5, 0.5, 0.0]] {
            let want = cms.cmyk_to_srgb(cmyk, false);
            let got = p.to_srgb(&cmyk, Intent::RelativeColorimetric).unwrap();
            let de = delta_e2000(lab::srgb_to_lab(want), lab::srgb_to_lab(got));
            assert!(de < 2.5, "{name} {cmyk:?}: {want:?} vs {got:?} (ΔE {de})");
        }
        // And back: an in-gamut colour separates to about the same inks.
        let srgb = cms.cmyk_to_srgb([0.1, 0.6, 0.8, 0.0], false);
        let back = p.from_srgb(srgb, Intent::RelativeColorimetric).unwrap();
        let again = cms.cmyk_to_srgb([back[0], back[1], back[2], back[3]], false);
        assert!(delta_e2000(lab::srgb_to_lab(srgb), lab::srgb_to_lab(again)) < 3.0, "{back:?}");
    }
}

#[test]
fn gray_is_srgb_toned() {
    let p = IccProfile::from_bytes(None, &icc_bytes(GRAY).unwrap()).unwrap();
    assert_eq!((p.name.as_str(), p.kind), (GRAY, ProfileKind::Gray));
    for v in [0.0f32, 0.25, 0.5, 1.0] {
        let got = p.to_srgb(&[v], Intent::RelativeColorimetric).unwrap();
        assert!(got.iter().all(|c| (c - v).abs() < 0.01), "{v} → {got:?}");
    }
}

#[test]
fn encoded_profiles_are_reproducible() {
    assert_eq!(icc::builtin_rgb_bytes(DISPLAY_P3).unwrap(), icc::builtin_rgb_bytes(DISPLAY_P3).unwrap());
}

#[test]
fn a_loaded_profile_is_embedded_as_loaded() {
    let mut bytes = icc::builtin_rgb_bytes(PROPHOTO_RGB).unwrap();
    // A byte the parser ignores (in the reserved header area) marks the file as this one.
    bytes[100] = 7;
    register_icc(&bytes, Some("Embedded As Loaded RGB".into())).unwrap();
    assert_eq!(&*icc_bytes("Embedded As Loaded RGB").unwrap(), &bytes[..]);
}

/// Settings with an ICC CMYK profile (Generic CMYK's, loaded as a file is) and Wide Gamut RGB.
fn icc_cmyk(bpc: bool) -> Cms {
    let name = register_icc(&icc_bytes(GENERIC_CMYK).unwrap(), Some("Test ICC CMYK".into())).unwrap().name;
    Cms::new(&ColorSettings { rgb: WIDE_GAMUT_RGB.into(), cmyk: name, bpc, ..ColorSettings::default() }).unwrap()
}

#[test]
fn icc_cmyk_converts_into_a_wide_rgb_space_directly_without_clipping_to_srgb() {
    let wide = icc_cmyk(true);
    let cyan = [1.0, 0.0, 0.0, 0.0];
    let direct = wide.cmyk_to_rgb(cyan, Intent::RelativeColorimetric);
    // Through sRGB, cyan clips to sRGB's gamut first: in the wider space that reads as a red
    // channel well above zero.
    let hub = wide.srgb_to_rgb(wide.cmyk_to_srgb(cyan, false));
    assert!(direct[0] + 0.15 < hub[0], "direct {direct:?}, through sRGB {hub:?}");
    // Same colour either way where sRGB holds it: a mid grey and paper white.
    for inks in [[0.0, 0.0, 0.0, 0.5], [0.0; 4]] {
        let (d, h) = (wide.cmyk_to_rgb(inks, Intent::RelativeColorimetric), wide.srgb_to_rgb(wide.cmyk_to_srgb(inks, false)));
        assert!(d.iter().zip(h).all(|(a, b)| (a - b).abs() < 0.02), "{inks:?}: direct {d:?}, through sRGB {h:?}");
    }
    // Cms::convert takes the direct route for CMYK.
    let Color::Rgb { r, .. } = wide.convert(&Color::Cmyk { c: 1.0, m: 0.0, y: 0.0, k: 0.0 }, Model::Rgb, Intent::RelativeColorimetric) else {
        panic!("RGB")
    };
    assert!((r - direct[0]).abs() < 1e-6);
}

#[test]
fn black_point_compensation_maps_the_darkest_ink_to_rgb_black() {
    let rich = [0.75, 0.68, 0.67, 0.9];
    let (on, off) = (icc_cmyk(true).cmyk_to_rgb(rich, Intent::RelativeColorimetric), icc_cmyk(false).cmyk_to_rgb(rich, Intent::RelativeColorimetric));
    // Without compensation the profile's black is a dark grey; with it, rich black goes most of
    // the way to RGB black (not all: it isn't quite the darkest ink the profile prints).
    assert!(off.iter().all(|v| *v > 0.1), "without: {off:?}");
    assert!(on.iter().zip(off).all(|(a, b)| *a < 0.08 && *a < b * 0.5), "with: {on:?}, without: {off:?}");
    // The screen shows what an sRGB conversion writes, compensated alike.
    let srgb = |bpc| {
        let c = icc_cmyk(bpc);
        Cms::new(&ColorSettings { rgb: SRGB.into(), ..c.settings().clone() }).unwrap()
    };
    for bpc in [true, false] {
        let c = srgb(bpc);
        assert_eq!(c.cmyk_to_srgb(rich, false), c.cmyk_to_rgb(rich, Intent::RelativeColorimetric), "bpc {bpc}");
    }
    // White is untouched, and absolute colorimetric never compensates.
    let white = icc_cmyk(true).cmyk_to_rgb([0.0; 4], Intent::RelativeColorimetric);
    assert!(white.iter().all(|v| *v > 0.99), "{white:?}");
    let abs = |bpc| icc_cmyk(bpc).cmyk_to_rgb(rich, Intent::AbsoluteColorimetric);
    assert_eq!(abs(true), abs(false));
    // Perceptual tables already map black: compensation leaves them alone (as lcms and
    // Ghostscript do).
    let perceptual = |bpc| icc_cmyk(bpc).cmyk_to_rgb(rich, Intent::Perceptual);
    assert_eq!(perceptual(true), perceptual(false));
}

/// #1584: the profiles written here (built-in RGB spaces) have every tag on a 4-byte boundary and
/// a size that is a multiple of 4 (ICC.1 §7.1.2), and still read back as the same space.
#[test]
fn written_profiles_align_their_tags() {
    for name in [super::SRGB, super::DISPLAY_P3, super::WIDE_GAMUT_RGB, super::PROPHOTO_RGB] {
        let icc = super::icc_bytes(name).unwrap();
        let be = |at: usize| u32::from_be_bytes([icc[at], icc[at + 1], icc[at + 2], icc[at + 3]]) as usize;
        assert_eq!(be(0), icc.len(), "{name}: header size");
        assert_eq!(icc.len() % 4, 0, "{name}: {} bytes", icc.len());
        let n = be(128);
        for i in 0..n {
            let (off, size) = (be(132 + 12 * i + 4), be(132 + 12 * i + 8));
            assert_eq!(off % 4, 0, "{name}: tag {i} at {off}");
            assert!(off + size <= icc.len());
        }
        let back = moxcms::ColorProfile::new_from_slice(&icc).unwrap();
        let orig = moxcms::ColorProfile::new_srgb();
        if name == super::SRGB {
            assert_eq!((back.red_colorant, back.green_colorant, back.blue_colorant), (orig.red_colorant, orig.green_colorant, orig.blue_colorant));
        }
    }
}

/// #2707: the CMYK profiles written here carry every tag ICC.1 requires of an output profile (the
/// tables of all three intents and the gamut tag): Windows rejects one without them, and Photos
/// then can't show a CMYK TIFF that embeds it.
#[test]
fn cmyk_profiles_have_the_tags_of_an_output_profile() {
    for name in BUILTIN_CMYK {
        let icc = icc_bytes(name).unwrap();
        let be = |at: usize| u32::from_be_bytes([icc[at], icc[at + 1], icc[at + 2], icc[at + 3]]) as usize;
        let tags: Vec<&[u8]> = (0..be(128)).map(|i| &icc[132 + 12 * i..136 + 12 * i]).collect();
        for want in [b"desc", b"cprt", b"wtpt", b"A2B0", b"A2B1", b"A2B2", b"B2A0", b"B2A1", b"B2A2", b"gamt"] {
            assert!(tags.contains(&&want[..]), "{name}: no {}", String::from_utf8_lossy(want));
        }
        let p = parse(&icc);
        assert!(p.lut_a_to_b_saturation.is_some() && p.lut_b_to_a_saturation.is_some() && p.gamut.is_some(), "{name}");
    }
}

#[test]
fn icc_cmyk_converts_into_another_icc_cmyk_directly() {
    // The same profile under two names. Where sRGB holds a colour, the direct conversion and a
    // trip through sRGB agree (both carry the profile's own table round trip, black regenerated);
    // CMYK cyan, which sRGB can't hold, stays cyan directly and loses about 30% through sRGB.
    let bytes = icc_bytes(GENERIC_CMYK).unwrap();
    let a = register_icc(&bytes, Some("Test ICC CMYK A".into())).unwrap().name;
    let b = register_icc(&bytes, Some("Test ICC CMYK B".into())).unwrap().name;
    let cms = |cmyk: &str, intent| Cms::new(&ColorSettings { cmyk: cmyk.into(), intent, bpc: false, ..ColorSettings::default() }).unwrap();
    for intent in [Intent::Perceptual, Intent::RelativeColorimetric] {
        let (src, dest) = (cms(&a, intent), cms(&b, intent));
        for inks in [[0.0, 1.0, 1.0, 0.0], [0.2, 0.4, 0.6, 0.1], [0.0, 0.0, 0.0, 0.5]] {
            let direct = src.cmyk_into(&dest, inks, intent).expect("both ICC");
            let via = dest.srgb_to_cmyk(src.cmyk_to_srgb(inks, false), intent);
            assert!(direct.iter().zip(via).all(|(x, y)| (x - y).abs() < 0.03), "{intent:?} {inks:?}: {direct:?} vs {via:?}");
        }
        let cyan = [1.0, 0.0, 0.0, 0.0];
        let direct = src.cmyk_into(&dest, cyan, intent).unwrap();
        let via = dest.srgb_to_cmyk(src.cmyk_to_srgb(cyan, false), intent);
        assert!(direct[0] > 0.9 && via[0] < 0.8, "{intent:?}: direct {direct:?}, through sRGB {via:?}");
    }
    // Perceptual tables map black themselves: compensation leaves them alone.
    let p = |bpc| {
        let s = Cms::new(&ColorSettings { cmyk: a.clone(), intent: Intent::Perceptual, bpc, ..ColorSettings::default() }).unwrap();
        let d = Cms::new(&ColorSettings { cmyk: b.clone(), intent: Intent::Perceptual, bpc, ..ColorSettings::default() }).unwrap();
        s.cmyk_into(&d, [0.75, 0.68, 0.67, 0.9], Intent::Perceptual)
    };
    assert_eq!(p(true), p(false));
    // The built-in spaces aren't ICC profiles: no direct route.
    assert!(Cms::default().cmyk_into(&Cms::default(), [1.0, 0.0, 0.0, 0.0], Intent::Perceptual).is_none());
    // RGB and display sRGB go straight into ICC CMYK too.
    let dest = cms(&a, Intent::Perceptual);
    assert!(dest.rgb_into_cmyk(&dest, [1.0, 0.0, 0.0], Intent::Perceptual).is_some());
    assert!(dest.srgb_into_cmyk([0.0, 0.0, 1.0], Intent::Perceptual).is_some());
}

#[test]
fn direct_cmyk_compensation_only_remaps_relative_black() {
    let src = IccProfile::from_bytes(None, &icc_bytes(SRGB).unwrap()).unwrap();
    let dest = IccProfile::from_bytes(None, &icc_bytes(GENERIC_CMYK).unwrap()).unwrap();
    let convert =
        |intent, bpc, rgb: [f32; 3]| src.to_cmyk_of(&dest, &rgb, intent, bpc).unwrap_or_else(|| panic!("no transform for {intent:?}, BPC {bpc}"));
    let relative = Intent::RelativeColorimetric;
    let on = convert(relative, true, [0.05; 3]);
    let off = convert(relative, false, [0.05; 3]);
    assert!(on.iter().zip(off).any(|(a, b)| (a - b).abs() > 0.01), "on {on:?}, off {off:?}");
    assert_eq!(on, convert(relative, true, [0.05; 3]), "cached intent/BPC key");
    assert_eq!(convert(relative, true, [1.0; 3]), convert(relative, false, [1.0; 3]));
    for intent in [Intent::Perceptual, Intent::AbsoluteColorimetric] {
        assert_eq!(convert(intent, true, [0.05; 3]), convert(intent, false, [0.05; 3]));
    }
    // The generated CMYK profiles carry a saturation table (#2707): saturation converts directly.
    for bpc in [true, false] {
        assert!(src.to_cmyk_of(&dest, &[0.05; 3], Intent::Saturation, bpc).is_some());
    }
    assert!(src.to_cmyk_of(&src, &[0.0; 3], relative, true).is_none());
}

#[test]
fn rgb_profiles_are_the_same_when_their_colours_are() {
    let srgb = icc_bytes(SRGB).unwrap();
    let wide = icc_bytes(WIDE_GAMUT_RGB).unwrap();
    let p3 = icc_bytes(DISPLAY_P3).unwrap();
    assert!(same_rgb_profile(&wide, &wide));
    // The same colorimetry in another file (here: another creation date) is the same profile.
    let mut again = wide.to_vec();
    again[24..26].copy_from_slice(&2001u16.to_be_bytes());
    assert_ne!(&*again, &*wide, "a different file");
    assert!(same_rgb_profile(&again, &wide));
    for (a, b) in [(&srgb, &wide), (&wide, &p3), (&p3, &srgb)] {
        assert!(!same_rgb_profile(a, b));
    }
    let cmyk = icc_bytes(GENERIC_CMYK).unwrap();
    assert!(!same_rgb_profile(&cmyk, &wide), "not RGB");
    assert!(!same_rgb_profile(b"not a profile", &wide));
}
