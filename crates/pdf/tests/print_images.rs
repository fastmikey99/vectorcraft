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
