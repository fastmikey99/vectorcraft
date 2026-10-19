#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Original hand-authored ISO fixtures. No third-party artwork or sample PDF bytes.
use vectorcraft_pdf::print_order;
fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
}
fn pdf(content: &str, resources: &str, extras: &[String]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << {resources} >> /Contents 4 0 R >>"),
        stream("", content),
    ];
    objects.extend_from_slice(extras);
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{object}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).bytes());
    for n in offsets.iter().skip(1) {
        out.extend(format!("{n:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len()).bytes());
    out
}
fn image(extra: &str) -> String {
    stream(&format!("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 {extra}"), "x")
}
#[test]
fn image_uses_retain_sequence_forms_resources_transforms_and_state() {
    let form = stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Matrix [2 0 0 3 4 5]", "/Im Do");
    let bytes = pdf(
        "0 0 1 1 re f q 10 0 0 20 30 40 cm /GS gs /Form Do Q /Im Do 0 0 1 1 re f /Form Do",
        "/XObject << /Im 5 0 R /Form 6 0 R /Unused 7 0 R >> /ColorSpace << /Photo /DeviceCMYK >> /ExtGState << /GS << /op true /OPM 1 >> >>",
        &[image("/ColorSpace /Photo"), form, image("/ColorSpace /DeviceRGB")],
    );
    let audit = print_order(&bytes).unwrap();
    let uses = &audit.images[0];
    assert_eq!(uses.len(), 3); // repeated paints count; unused resources do not
    assert_eq!(uses.iter().map(|i| i.before_path).collect::<Vec<_>>(), [1, 1, 2]);
    assert_eq!(uses[0].matrix, Some([20., 0., 0., 60., 70., 140.]));
    assert_eq!(uses[1].matrix, Some([1., 0., 0., 1., 0., 0.]));
    assert_eq!(uses[2].matrix, Some([2., 0., 0., 3., 4., 5.]));
    assert!(uses[0].clip_context && uses[2].clip_context && !uses[1].clip_context);
    assert!(uses[0].overprint && !uses[1].overprint);
    assert_eq!(uses[0].overprint_mode, 1);
    assert_eq!(uses[1].overprint_mode, 0);
    assert!(uses.iter().all(|i| i.description.colour_space == "DeviceCMYK" && !i.inline));
    assert!(audit.issues.iter().any(|s| s.contains("coverage requires independent")));
}
#[test]
fn inline_aliases_and_clips_are_audited_without_pixel_bounds_becoming_coverage() {
    let bytes = pdf("q 0 0 1 1 re W n BI /W 1 /H 1 /BPC 8 /CS /RGB /I true /D [1 0 0 1 0 1] ID abc EI Q BI /W 1 /H 1 /IM true ID x EI", "", &[]);
    let audit = print_order(&bytes).unwrap();
    assert_eq!(audit.images[0].len(), 2);
    let a = &audit.images[0][0];
    assert!(a.inline && a.clip_context && a.description.interpolate);
    assert_eq!(a.description.dimensions, Some([1, 1]));
    assert_eq!(a.description.colour_space, "DeviceRGB");
    assert_eq!(a.description.decode, Some(vec![1., 0., 0., 1., 0., 1.]));
    let b = &audit.images[0][1];
    assert!(b.description.stencil && !b.clip_context);
    assert_eq!(b.description.bits_per_component, Some(1));
    assert_eq!(b.description.colour_space, "stencil_current_fill");
    assert!(audit.pages[0].is_empty());
}
#[test]
fn duplicate_inline_aliases_are_unresolved_even_when_values_agree() {
    let bytes = pdf(
        "BI /Width 1 /W 1 /Height 1 /H 1 /BitsPerComponent 8 /BPC 8 /ColorSpace /G /CS /G /Interpolate false /I true /Decode [0 1] /D [1 0] ID x EI",
        "",
        &[],
    );
    let audit = print_order(&bytes).unwrap();
    assert_eq!(audit.images[0].len(), 1);
    assert_eq!(audit.images[0][0].description.issues.iter().filter(|s| s.contains("Ambiguous inline image aliases")).count(), 6);
}
#[test]
fn mask_dependencies_are_not_counted_as_extra_paints_and_exact_names_are_retained() {
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[
            image("/ColorSpace [/DeviceN [/Cyan /Original#20ink#ff] /DeviceCMYK 7 0 R] /Mask 6 0 R /SMask 6 0 R /SMaskInData 1 /OC 8 0 R"),
            image("/ColorSpace /DeviceGray"),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 1] /N 1 >>".into(),
            "<< /Type /OCG /Name (Original layer) >>".into(),
        ],
    );
    let audit = print_order(&bytes).unwrap();
    assert_eq!(audit.images[0].len(), 1);
    let d = &audit.images[0][0].description;
    assert_eq!(d.colour_space, "DeviceN");
    assert_eq!(d.colourants_hex, ["4379616e", "4f726967696e616c20696e6bff"]);
    assert_eq!(d.mask, "image");
    assert!(d.soft_mask && d.embedded_alpha && d.optional_content);
    assert!(audit.issues.iter().any(|s| s.contains("Image optional-content")));
    assert_eq!(audit.images[0][0].layer.map(|i| audit.layers[i].as_str()), Some("Original layer"));
}
#[test]
fn malformed_description_and_default_substitution_remain_explicit() {
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >> /ColorSpace << /DefaultCMYK /DeviceRGB >>",
        &[stream("/Subtype /Image /Width -1 /Height 1 /ColorSpace /DeviceCMYK /Interpolate /Bad /Decode [0 /Bad]", "x")],
    );
    let audit = print_order(&bytes).unwrap();
    let d = &audit.images[0][0].description;
    assert!(d.dimensions.is_none() && d.bits_per_component.is_none() && d.decode.is_none());
    for text in ["dimensions", "boolean", "sample depth", "default substitution", "Decode"] {
        assert!(d.issues.iter().any(|s| s.contains(text)), "{text}: {:?}", d.issues);
    }
}
#[test]
fn excessive_image_uses_fail_instead_of_returning_a_partial_inventory() {
    let bytes = pdf(&"/Im Do ".repeat(4097), "/XObject << /Im 5 0 R >>", &[image("/ColorSpace /DeviceGray")]);
    assert!(print_order(&bytes).is_err());
}
#[test]
fn form_resource_override_is_local_and_missing_xobjects_are_visible() {
    let form = stream("/Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Im 7 0 R >> >>", "/Im Do");
    let bytes = pdf(
        "/Im Do /Form Do /Im Do /Missing Do",
        "/XObject << /Im 5 0 R /Form 6 0 R >>",
        &[image("/ColorSpace /DeviceGray"), form, image("/ColorSpace /DeviceRGB")],
    );
    let audit = print_order(&bytes).unwrap();
    assert_eq!(audit.images[0].iter().map(|i| i.description.colour_space.as_str()).collect::<Vec<_>>(), ["DeviceGray", "DeviceRGB", "DeviceGray"]);
    assert!(audit.issues.iter().any(|s| s.contains("Missing XObject")));
}

#[test]
fn embedded_alpha_zero_does_not_mean_an_alpha_channel_is_present() {
    for (value, enabled, invalid) in [("0", false, false), ("2", true, false), ("3", false, true), ("/Bad", false, true)] {
        let bytes = pdf("/Im Do", "/XObject << /Im 5 0 R >>", &[image(&format!("/ColorSpace /DeviceGray /SMaskInData {value}"))]);
        let audit = print_order(&bytes).unwrap();
        let d = &audit.images[0][0].description;
        assert_eq!(d.embedded_alpha, enabled);
        assert_eq!(d.issues.iter().any(|i| i.contains("SMaskInData")), invalid);
    }
}

#[test]
fn pattern_images_cannot_masquerade_as_a_complete_empty_image_census() {
    let pattern = stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1 /Resources << /XObject << /Im 6 0 R >> >>",
        "/Im Do",
    );
    let bytes = pdf("/Pattern cs /Tile scn 0 0 10 10 re f", "/Pattern << /Tile 5 0 R >>", &[pattern, image("/ColorSpace /DeviceGray")]);
    let audit = print_order(&bytes).unwrap();
    assert!(audit.images[0].is_empty());
    assert!(audit.issues.iter().any(|s| s.contains("nested composited paint census")));
}
#[test]
fn raw_gray_rows_decode_with_padding_and_keep_white_as_zero_ink() {
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode", "4080>")],
    );
    let audit = print_order(&bytes).unwrap();
    let samples = audit.images[0][0].samples.as_ref().unwrap();
    assert_eq!(samples.colourants, ["Black"]);
    assert_eq!(tones(samples), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(alphas(samples), [1.0, 1.0, 1.0, 1.0]);
    // Published v1 digest layout, independently evaluated from the four expected tones.
    assert_eq!(samples.sample_sha256, "896ec645ec8a5e1a522fd40a434c6650314bfe95bc7fa4a0c89a503ea1b0011b");
    assert!(audit.issues.iter().any(|s| s.contains("coverage requires independent")));
}
#[test]
fn indexed_lookup_is_source_ink_and_decode_can_reverse_indices() {
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[stream(
            "/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 1 /ColorSpace [/Indexed /DeviceCMYK 1 <ff00000000ff0000>] /Decode [1 0] /Filter /ASCIIHexDecode",
            "40>",
        )],
    );
    let audit = print_order(&bytes).unwrap();
    let samples = audit.images[0][0].samples.as_ref().unwrap();
    assert_eq!(tones(samples), [0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
    assert!(!samples.direct_cmyk);
}
#[test]
fn soft_mask_samples_and_color_key_masks_are_not_image_rectangles() {
    for (mask, extra) in [
        (
            "/SMask 6 0 R",
            Some(stream(
                "/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode",
                "00ff>",
            )),
        ),
        ("/Mask [0 0]", None),
    ] {
        let mut objects = vec![stream(
            &format!("/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray {mask} /Filter /ASCIIHexDecode"),
            "00ff>",
        )];
        objects.extend(extra);
        let bytes = pdf("/Im Do", "/XObject << /Im 5 0 R >>", &objects);
        let audit = print_order(&bytes).unwrap();
        let samples = audit.images[0][0].samples.as_ref().unwrap();
        assert_eq!(tones(samples), [1.0, 0.0]);
        assert_eq!(alphas(samples), [0.0, 1.0]);
        assert_eq!((samples.minimum_alpha, samples.maximum_alpha), (0.0, 1.0));
    }
}
#[test]
fn explicit_mask_decode_and_named_zero_components_survive() {
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[
            stream(
                "/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace [/DeviceN [/Cyan /Original#20ink] /DeviceCMYK 7 0 R] /Mask 6 0 R /Filter /ASCIIHexDecode",
                "00ffff00>",
            ),
            stream("/Type /XObject /Subtype /Image /Width 2 /Height 1 /ImageMask true /Decode [1 0] /Filter /ASCIIHexDecode", "40>"),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 0 0 1] /N 1 >>".into(),
        ],
    );
    let audit = print_order(&bytes).unwrap();
    let samples = audit.images[0][0].samples.as_ref().unwrap();
    assert_eq!(samples.colourants, ["Cyan", "Original ink"]);
    assert_eq!(tones(samples), [0.0, 1.0, 1.0, 0.0]);
    assert_eq!(alphas(samples), [0.0, 1.0]);
    assert!(!samples.direct_cmyk);
}
#[test]
fn invalid_masks_samples_and_budgets_never_become_empty_coverage() {
    for extra in ["/Decode [0]", "/Mask [0 300]", "/Mask 99 0 R", "/SMask 99 0 R", "/SMaskInData 1"] {
        let bytes = pdf(
            "/Im Do",
            "/XObject << /Im 5 0 R >>",
            &[stream(
                &format!(
                    "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray {extra} /Filter /ASCIIHexDecode"
                ),
                "ff>",
            )],
        );
        let audit = print_order(&bytes).unwrap();
        let image = &audit.images[0][0];
        assert!(image.samples.is_none(), "{extra}");
        assert!(image.sample_error.is_some(), "{extra}");
    }
}

#[test]
fn unqualified_index_depth_and_invalid_stencil_decode_are_unresolved() {
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[stream(
            "/Subtype /Image /Width 1 /Height 1 /BitsPerComponent 16 /ColorSpace [/Indexed /DeviceGray 1 <00ff>] /Filter /ASCIIHexDecode",
            "0000>",
        )],
    );
    assert!(print_order(&bytes).unwrap().images[0][0].sample_error.as_ref().unwrap().contains("16-bit"));
    for decode in ["[0 0.5]", "[0.5 1]", "[-1 1]"] {
        let bytes = pdf(
            "/Im Do",
            "/XObject << /Im 5 0 R >>",
            &[
                image("/ColorSpace /DeviceGray /Mask 6 0 R"),
                stream(&format!("/Subtype /Image /Width 1 /Height 1 /ImageMask true /Decode {decode}"), "x"),
            ],
        );
        assert!(print_order(&bytes).unwrap().images[0][0].sample_error.as_ref().unwrap().contains("binary Decode"));
    }
}
#[test]
fn soft_mask_default_gray_and_explicit_oversize_are_unresolved() {
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >> /ColorSpace << /DefaultGray /DeviceRGB >>",
        &[
            stream("/Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceCMYK /SMask 6 0 R", "xxxx"),
            image("/ColorSpace /DeviceGray"),
        ],
    );
    assert!(print_order(&bytes).unwrap().images[0][0].sample_error.as_ref().unwrap().contains("DefaultGray"));
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[stream("/Subtype /Image /Width 300000000 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray", "x")],
    );
    assert!(print_order(&bytes).unwrap().images[0][0].sample_error.as_ref().unwrap().contains("budget"));
}
#[test]
fn image_compositing_state_restores_after_graphics_pop() {
    for content in ["q /GS gs /Im Do Q /Im Do", "q /Perceptual ri /Im Do Q /Im Do"] {
        for gs in ["/BM /Multiply", "/SMask /None", "/HTP [0 0]"] {
            let bytes = pdf(content, &format!("/XObject << /Im 5 0 R >> /ExtGState << /GS << {gs} >> >>"), &[image("/ColorSpace /DeviceGray")]);
            let audit = print_order(&bytes).unwrap();
            assert!(audit.images[0][0].compositing_unresolved);
            assert!(!audit.images[0][1].compositing_unresolved);
        }
    }
}

#[test]
fn fractional_indexed_decode_is_unresolved_instead_of_a_false_tonal_failure() {
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[stream(
            "/Subtype /Image /Width 2 /Height 2 /BitsPerComponent 4 /ColorSpace [/Indexed /DeviceCMYK 3 <ff00000000ff00000000ff00000000ff>] /Decode [0 3] /Filter /ASCIIHexDecode",
            "2378>",
        )],
    );
    assert!(print_order(&bytes).unwrap().images[0][0].sample_error.as_ref().unwrap().contains("Fractional Indexed Decode"));
}

#[test]
fn sixteen_bit_color_key_mask_is_unresolved_instead_of_a_false_tonal_failure() {
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[stream(
            "/Subtype /Image /Width 2 /Height 2 /BitsPerComponent 16 /ColorSpace /DeviceGray /Mask [0 0] /Filter /ASCIIHexDecode",
            "0000000100ffffff>",
        )],
    );
    assert!(print_order(&bytes).unwrap().images[0][0].sample_error.as_ref().unwrap().contains("16-bit colour-key"));
}

fn tones(samples: &vectorcraft_pdf::PrintImageSamples) -> Vec<f64> {
    (0..samples.width as usize * samples.height as usize)
        .flat_map(|pixel| (0..samples.colourants.len()).map(move |channel| samples.tone(pixel, channel).unwrap()))
        .collect()
}
fn alphas(samples: &vectorcraft_pdf::PrintImageSamples) -> Vec<f64> {
    (0..samples.width as usize * samples.height as usize).map(|pixel| samples.alpha(pixel).unwrap()).collect()
}
#[test]
fn production_sized_cmyk_stays_packed_without_losing_decode_precision() {
    let pixels = 2000 * 2000;
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[
            stream(
                "/Subtype /Image /Width 2000 /Height 2000 /BitsPerComponent 8 /ColorSpace /DeviceCMYK /Decode [0.123456789012345 0.876543210987654 0 1 0 1 0 1] /SMask 6 0 R",
                &"x".repeat(pixels * 4),
            ),
            stream("/Subtype /Image /Width 2000 /Height 2000 /BitsPerComponent 8 /ColorSpace /DeviceGray", &"x".repeat(pixels)),
        ],
    );
    let audit = print_order(&bytes).unwrap();
    let s = audit.images[0][0].samples.as_ref().unwrap();
    assert_eq!(s.retained_sample_bytes(), pixels * 5);
    let expected = 0.123456789012345 + (120.0 / 255.0) * (0.876543210987654 - 0.123456789012345);
    assert_eq!(s.tone(0, 0).unwrap(), expected);
    assert_eq!(s.tone(pixels - 1, 0).unwrap(), expected);
    assert_eq!(s.alpha(pixels - 1).unwrap(), 120.0 / 255.0);
    assert!(s.tone(pixels, 0).is_err());
    assert!(s.tone(0, 4).is_err());
    assert!(s.alpha(pixels).is_err());
}

#[test]
fn compressed_images_and_palettes_cannot_be_accepted_as_truncated_prefixes() {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&[0, 255, 64]).unwrap();
    let hex: String = encoder.finish().unwrap().iter().map(|b| format!("{b:02x}")).collect();
    let data = format!("{hex}>");
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[stream("/Subtype /Image /Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter [/ASCIIHexDecode /FlateDecode]", &data)],
    );
    assert!(print_order(&bytes).unwrap().images[0][0].sample_error.as_ref().unwrap().contains("length"));
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[
            stream("/Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace [/Indexed /DeviceGray 1 6 0 R]", "x"),
            stream("/Filter [/ASCIIHexDecode /FlateDecode]", &data),
        ],
    );
    assert!(print_order(&bytes).unwrap().images[0][0].sample_error.as_ref().unwrap().contains("length"));
}

#[test]
fn inline_gray_samples_decode_without_the_ei_separator() {
    for (data, first, second) in [("Az", 65.0, 122.0), ("A ", 65.0, 32.0), (" A", 32.0, 65.0)] {
        for separator in [" ", "\r\n"] {
            let bytes = pdf(&format!("BI /W 2 /H 1 /BPC 8 /CS /G ID {data}{separator}EI"), "", &[]);
            let audit = print_order(&bytes).unwrap();
            let image = &audit.images[0][0];
            assert!(image.sample_error.is_none(), "{:?}", image.sample_error);
            assert_eq!(tones(image.samples.as_ref().unwrap()), [1.0 - first / 255.0, 1.0 - second / 255.0]);
        }
    }
    let short = pdf("BI /W 3 /H 1 /BPC 8 /CS /G ID A EI", "", &[]);
    assert!(print_order(&short).unwrap().images[0][0].sample_error.as_ref().unwrap().contains("shorter"));
    for data in ["A", "AzX"] {
        let bytes = pdf(&format!("BI /W 2 /H 1 /BPC 8 /CS /G ID {data} EI"), "", &[]);
        assert!(print_order(&bytes).unwrap().images[0][0].samples.is_none());
    }
}

#[test]
fn packed_two_four_and_sixteen_bit_rows_keep_padding_and_source_precision() {
    for (bits, width, height, data, want) in [
        (2, 3, 2, "1be7>", vec![1.0, 1.0 - 1.0 / 3.0, 1.0 - 2.0 / 3.0, 0.0, 1.0 - 2.0 / 3.0, 1.0 - 1.0 / 3.0]),
        (4, 3, 2, "012ffed9>", vec![1.0, 1.0 - 1.0 / 15.0, 1.0 - 2.0 / 15.0, 0.0, 1.0 - 14.0 / 15.0, 1.0 - 13.0 / 15.0]),
        (16, 1, 2, "0001abcd>", vec![1.0 - 1.0 / 65535.0, 1.0 - 43981.0 / 65535.0]),
    ] {
        let bytes = pdf(
            "/Im Do",
            "/XObject << /Im 5 0 R >>",
            &[stream(
                &format!("/Subtype /Image /Width {width} /Height {height} /BitsPerComponent {bits} /ColorSpace /DeviceGray /Filter /ASCIIHexDecode"),
                data,
            )],
        );
        let audit = print_order(&bytes).unwrap();
        let s = audit.images[0][0].samples.as_ref().unwrap();
        assert_eq!(tones(s), want, "{bits}-bit");
    }
}
#[test]
fn refused_lossy_codec_does_not_spend_a_decode_budget_it_never_uses() {
    let bytes = pdf(
        "/Big Do /Small Do",
        "/XObject << /Big 5 0 R /Small 6 0 R >>",
        &[
            stream("/Subtype /Image /Width 8192 /Height 8192 /BitsPerComponent 8 /ColorSpace /DeviceCMYK /Filter /DCTDecode", "not a JPEG"),
            image("/ColorSpace /DeviceGray"),
        ],
    );
    let audit = print_order(&bytes).unwrap();
    assert!(audit.images[0][0].sample_error.as_ref().unwrap().contains("compression"));
    assert!(audit.images[0][1].samples.is_some());
}
#[test]
fn compressed_stream_cut_midway_never_yields_complete_samples() {
    use std::io::Write;
    let original: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&original).unwrap();
    let mut compressed = encoder.finish().unwrap();
    compressed.truncate(compressed.len() / 2);
    let hex: String = compressed.iter().map(|b| format!("{b:02x}")).collect();
    let bytes = pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[stream(
            "/Subtype /Image /Width 256 /Height 4 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter [/ASCIIHexDecode /FlateDecode]",
            &format!("{hex}>"),
        )],
    );
    let audit = print_order(&bytes).unwrap();
    assert!(audit.images[0][0].samples.is_none());
    assert!(audit.images[0][0].sample_error.is_some());
}

#[test]
fn explicit_image_intents_retain_samples_without_qualifying_the_output() {
    use vectorcraft_pdf::PrintRenderingIntent as Intent;
    for (space, data, expected) in [
        ("/DeviceGray", "00ff>", vec![1., 0.]),
        ("/DeviceCMYK", "ff00000000ff0000>", vec![1., 0., 0., 0., 0., 1., 0., 0.]),
        ("[/Indexed /DeviceCMYK 1 <ff00000000ff0000>]", "0001>", vec![1., 0., 0., 0., 0., 1., 0., 0.]),
    ] {
        let mut hashes = vec![];
        for (entry, intent) in [
            ("", Intent::RelativeColorimetric),
            ("/Intent /RelativeColorimetric", Intent::RelativeColorimetric),
            ("/Intent /AbsoluteColorimetric", Intent::AbsoluteColorimetric),
            ("/Intent /Perceptual", Intent::Perceptual),
            ("/Intent /Saturation", Intent::Saturation),
        ] {
            let bytes = pdf(
                "/Im Do",
                "/XObject << /Im 5 0 R >>",
                &[stream(
                    &format!("/Subtype /Image /Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace {space} /Filter /ASCIIHexDecode {entry}"),
                    data,
                )],
            );
            let raw = print_order(&bytes).unwrap();
            let use_ = &raw.images[0][0];
            assert_eq!(use_.effective_rendering_intent, intent);
            assert_eq!(use_.description.rendering_intent, (!entry.is_empty()).then_some(intent));
            assert_eq!(use_.rendering_intent_explicit, !entry.is_empty());
            assert_eq!(use_.compositing_unresolved, !entry.is_empty());
            let samples = use_.samples.as_ref().unwrap();
            assert_eq!(tones(samples), expected);
            assert_eq!(alphas(samples), [1., 1.]);
            hashes.push(samples.sample_sha256.clone());
        }
        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
    }
}

#[test]
fn image_intent_overrides_inherited_state_and_q_restores_it() {
    use vectorcraft_pdf::PrintRenderingIntent as I;
    let bytes = pdf(
        "/Im Do /Perceptual ri /Im Do q /GS gs /Im Do /Own Do Q /Im Do q /Bad gs /Im Do /Own Do Q /Im Do",
        "/XObject << /Im 5 0 R /Own 6 0 R >> /ExtGState << /GS << /RI /Saturation /ca 0.5 >> /Bad << /RI 17 >> >>",
        &[image("/ColorSpace /DeviceGray"), image("/ColorSpace /DeviceGray /Intent /AbsoluteColorimetric")],
    );
    let raw = print_order(&bytes).unwrap();
    let uses = &raw.images[0];
    assert_eq!(
        uses.iter().map(|i| i.effective_rendering_intent).collect::<Vec<_>>(),
        [
            I::RelativeColorimetric,
            I::Perceptual,
            I::Saturation,
            I::AbsoluteColorimetric,
            I::Perceptual,
            I::Unresolved,
            I::AbsoluteColorimetric,
            I::Perceptual
        ]
    );
    assert!(!uses[0].compositing_unresolved && !uses[0].rendering_intent_explicit);
    assert!(uses[1..].iter().all(|i| i.compositing_unresolved && i.rendering_intent_explicit && i.samples.is_some()));
    assert!(uses.iter().filter(|i| i.description.rendering_intent.is_some()).all(|i| i.compositing_unresolved));
    let restored = print_order(&pdf("q /Perceptual ri /Im Do Q /Im Do", "/XObject << /Im 5 0 R >>", &[image("/ColorSpace /DeviceGray")])).unwrap();
    assert_eq!(restored.images[0][1].effective_rendering_intent, I::RelativeColorimetric);
    assert!(!restored.images[0][1].compositing_unresolved && !restored.images[0][1].rendering_intent_explicit);
}

#[test]
fn invalid_intent_and_alternate_sources_cannot_gain_decoded_evidence() {
    use vectorcraft_pdf::PrintRenderingIntent as I;
    for entry in [
        "/Intent /Unknown",
        "/Intent (Perceptual)",
        "/Intent null",
        "/Intent 42",
        "/Intent /Perceptual /Alternates []",
        "/Intent /Perceptual /OPI << >>",
    ] {
        let raw = print_order(&pdf("/Im Do", "/XObject << /Im 5 0 R >>", &[image(&format!("/ColorSpace /DeviceGray {entry}"))])).unwrap();
        let use_ = &raw.images[0][0];
        assert!(use_.samples.is_none() && use_.sample_error.is_some() && use_.compositing_unresolved, "{entry}");
        if !entry.contains("/Perceptual /") {
            assert_eq!(use_.description.rendering_intent, Some(I::Unresolved));
        }
    }
    let raw = print_order(&pdf("/Unknown ri /Im Do", "/XObject << /Im 5 0 R >>", &[image("/ColorSpace /DeviceGray")])).unwrap();
    assert_eq!(raw.images[0][0].effective_rendering_intent, I::Unresolved);
    assert!(raw.images[0][0].compositing_unresolved);
}

#[test]
fn mask_intent_samples_retain_a_separate_unqualified_declaration() {
    for (key, mask_dict) in [("SMask", "/ColorSpace /DeviceGray /BitsPerComponent 8"), ("Mask", "/ImageMask true /BitsPerComponent 1")] {
        for entry in [
            "",
            "/Intent /RelativeColorimetric",
            "/Intent /AbsoluteColorimetric",
            "/Intent /Perceptual",
            "/Intent /Saturation",
            "/Intent /Unknown",
            "/Intent 42",
        ] {
            let raw = print_order(&pdf(
                "/Im Do",
                "/XObject << /Im 5 0 R >>",
                &[
                    image(&format!("/ColorSpace /DeviceGray /Intent /Perceptual /{key} 6 0 R")),
                    stream(&format!("/Subtype /Image /Width 1 /Height 1 {mask_dict} {entry}"), "x"),
                ],
            ))
            .unwrap();
            let use_ = &raw.images[0][0];
            assert_eq!(use_.samples.is_some(), !matches!(entry, "/Intent /Unknown" | "/Intent 42"), "{key} {entry}");
            assert_eq!(use_.description.mask_rendering_intent.is_some(), !entry.is_empty());
            assert!(use_.compositing_unresolved);
        }
    }
    let raw = print_order(&pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >> /ColorSpace << /DefaultGray /DeviceRGB >>",
        &[image("/ColorSpace /DeviceGray /Intent /Perceptual")],
    ))
    .unwrap();
    assert!(raw.images[0][0].samples.is_none());
    assert!(raw.images[0][0].description.issues.iter().any(|i| i.contains("default substitution")));
}

#[test]
fn inline_and_form_intent_scopes_are_retained() {
    use vectorcraft_pdf::PrintRenderingIntent as I;
    let form = stream("/Subtype /Form /BBox [0 0 1 1]", "/Saturation ri /Im Do");
    let raw = print_order(&pdf(
        "/Perceptual ri /Form Do /Im Do BI /W 1 /H 1 /BPC 8 /CS /G /Intent /AbsoluteColorimetric ID x EI",
        "/XObject << /Im 5 0 R /Form 6 0 R >>",
        &[image("/ColorSpace /DeviceGray"), form],
    ))
    .unwrap();
    assert_eq!(
        raw.images[0].iter().map(|i| i.effective_rendering_intent).collect::<Vec<_>>(),
        [I::Saturation, I::Perceptual, I::AbsoluteColorimetric]
    );
    assert!(raw.images[0].iter().all(|i| i.compositing_unresolved && i.samples.is_some()));
}

#[test]
fn mask_intent_alone_keeps_sample_consumers_unqualified() {
    let raw = print_order(&pdf(
        "/Im Do",
        "/XObject << /Im 5 0 R >>",
        &[image("/ColorSpace /DeviceGray /SMask 6 0 R"), image("/ColorSpace /DeviceGray /Intent /Perceptual")],
    ))
    .unwrap();
    let use_ = &raw.images[0][0];
    assert!(use_.samples.is_some() && use_.compositing_unresolved);
    assert!(use_.description.rendering_intent.is_none() && !use_.rendering_intent_explicit);
    assert_eq!(use_.description.mask_rendering_intent, Some(vectorcraft_pdf::PrintRenderingIntent::Perceptual));
}

#[test]
fn implicit_intent_metadata_keeps_existing_preparation_serialization() {
    let raw = print_order(&pdf("/Im Do", "/XObject << /Im 5 0 R >>", &[image("/ColorSpace /DeviceGray")])).unwrap();
    let serialized = serde_json::to_value(&raw.images[0][0]).unwrap();
    assert!(serialized.get("effective_rendering_intent").is_none());
    assert!(serialized.get("rendering_intent_explicit").is_none());
    assert!(serialized["description"].get("rendering_intent").is_none());
    assert!(serialized["description"].get("mask_rendering_intent").is_none());
    let explicit = print_order(&pdf("/RelativeColorimetric ri /Im Do", "/XObject << /Im 5 0 R >>", &[image("/ColorSpace /DeviceGray")])).unwrap();
    let serialized = serde_json::to_value(&explicit.images[0][0]).unwrap();
    assert_eq!(serialized["rendering_intent_explicit"], true);
    assert_eq!(serialized["compositing_unresolved"], true);
}

#[test]
fn unresolved_extgstate_is_not_silently_treated_as_the_previous_state() {
    for entry in ["", "/Bad 42", "/Bad []", "/Bad (wrong)", "/Bad 99 0 R"] {
        let bytes =
            pdf("/Bad gs /Im Do 0 0 1 1 re f", &format!("/XObject << /Im 5 0 R >> /ExtGState << {entry} >>"), &[image("/ColorSpace /DeviceGray")]);
        let raw = print_order(&bytes).unwrap();
        assert!(raw.issues.iter().any(|i| i.contains("Missing or malformed extended graphics-state")), "{entry}");
        let use_ = &raw.images[0][0];
        assert!(use_.samples.is_some() && use_.compositing_unresolved);
        assert_eq!(use_.effective_rendering_intent, vectorcraft_pdf::PrintRenderingIntent::Unresolved);
        assert!(raw.issues.iter().any(|i| i.contains("Paint transfer")));
    }
}

#[test]
fn malformed_overprint_types_and_modes_remain_unresolved() {
    for entries in ["/OP 1", "/op /Wrong", "/OP null", "/op []", "/OPM 2", "/OPM -1", "/OPM 0.5", "/OPM 0.0", "/OPM 1.0", "/OPM /Wrong", "/OPM true"]
    {
        let raw = print_order(&pdf(
            "/Bad gs /Im Do 0 0 1 1 re B",
            &format!("/XObject << /Im 5 0 R >> /ExtGState << /Bad << {entries} >> >>"),
            &[image("/ColorSpace /DeviceGray")],
        ))
        .unwrap();
        assert!(raw.issues.iter().any(|i| i.contains("Malformed graphics-state overprint")), "{entries}");
        assert!(raw.images[0][0].compositing_unresolved && raw.images[0][0].samples.is_some());
        assert!(raw.issues.iter().any(|i| i.contains("Paint transfer")));
    }
    for (entries, fill, mode) in
        [("", false, 0), ("/OP true", true, 0), ("/OP true /op false /OPM 1", false, 1), ("/OP false /op true /OPM 0", true, 0)]
    {
        let raw = print_order(&pdf(
            "/Good gs /Im Do",
            &format!("/XObject << /Im 5 0 R >> /ExtGState << /Good << {entries} >> >>"),
            &[image("/ColorSpace /DeviceGray")],
        ))
        .unwrap();
        let use_ = &raw.images[0][0];
        assert!(!use_.compositing_unresolved, "{entries}");
        assert_eq!((use_.overprint, use_.overprint_mode), (fill, mode));
        assert_eq!(raw.issues.len(), 1); // Only the ordinary independent-image reconciliation note.
    }
}

#[test]
fn unresolved_extgstate_restores_locally_but_retains_the_document_issue() {
    let form = stream("/Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Im 5 0 R >> /ExtGState << /Bad 42 >> >>", "/Bad gs /Im Do");
    let raw = print_order(&pdf(
        "q /Missing gs /Im Do Q /Im Do /Form Do /Im Do",
        "/XObject << /Im 5 0 R /Form 6 0 R >>",
        &[image("/ColorSpace /DeviceGray"), form],
    ))
    .unwrap();
    assert_eq!(raw.images[0].iter().map(|i| i.compositing_unresolved).collect::<Vec<_>>(), [true, false, true, false]);
    assert!(raw.issues.iter().any(|i| i.contains("extended graphics-state")));
}
