//! Exact FormBBox containment, state restoration and refusal controls.
use serde_json::json;

fn stream(entries: &str, content: &str) -> String {
    format!("<< {entries} /Length {} >>\nstream\n{content}\nendstream", content.len())
}
fn fixture(bbox: &str, matrix: &str, page: &str, form: &str, inner_bbox: &str, inner: &str, tail: &str) -> Vec<u8> {
    crate::pdf::tests::objects_pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Resources << /XObject << /Im 5 0 R /F 6 0 R /I 7 0 R >> >> /Contents [4 0 R 8 0 R] >>"
            .into(),
        stream("", page),
        stream(
            "/Type /XObject /Subtype /Image /Width 3 /Height 3 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode",
            "0020406080a0c0e0ff>",
        ),
        stream(&format!("/Type /XObject /Subtype /Form /BBox [{bbox}] /Resources << /XObject << /Im 5 0 R /I 7 0 R >> >> {matrix}"), form),
        stream(&format!("/Type /XObject /Subtype /Form /BBox [{inner_bbox}] /Resources << /XObject << /Im 5 0 R >> >>"), inner),
        stream("", tail),
    ])
}
const IMAGE: &str = "q 36 0 0 36 18 18 cm /Im Do Q";

#[test]
fn exact_form_clips_preserve_census_and_refuse_unknown_or_cropping_contexts() {
    let cases = [
        ("dangling-box-tail", "0 0 72 72 999 0 R", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("dangling-matrix-tail", "0 0 72 72", "/Matrix [1 0 0 1 0 0 999 0 R]", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("containing", "0 0 72 72", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", true),
        ("large-export", "-32767 -32767 32767 32767", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", true),
        ("inside-unit", "17 17 55 55", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", true),
        ("touch", "18 18 54 54", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("crop", "19 18 54 54", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("empty", "0 0 0 72", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("reversed", "72 0 0 72", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("real-box", "0.0 0 72 72", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("oversize-box", "-1048577 0 72 72", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("reflection", "0 0 72 72", "/Matrix [-1 0 0 1 72 0]", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", true),
        ("translation", "0 0 60 60", "/Matrix [1 0 0 1 1 2]", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", true),
        ("form-shear", "0 0 72 72", "/Matrix [1 0 1 1 0 0]", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("form-rotation", "0 0 72 72", "/Matrix [0 1 -1 0 72 0]", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("real-matrix", "0 0 72 72", "/Matrix [1.0 0 0 1 0 0]", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("degenerate", "0 0 72 72", "/Matrix [0 0 0 1 0 0]", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("nested", "0 0 72 72", "", "/F Do", "/I Do", "0 0 60 60", IMAGE, "", true),
        ("nested-crop", "0 0 72 72", "", "/F Do", "/I Do", "0 0 20 60", IMAGE, "", false),
        ("outer-crop", "0 0 20 60", "", "/F Do", "/I Do", "0 0 72 72", IMAGE, "", false),
        ("path-before", "0 0 72 72", "", "0 0 72 72 re W n /F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("evenodd-before", "0 0 72 72", "", "0 0 72 72 re W* n /F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("nested-unknown", "0 0 72 72", "", "/F Do", "0 0 72 72 re W n /I Do", "0 0 60 60", IMAGE, "", false),
        ("restored-path", "0 0 72 72", "", "q 0 0 1 1 re W n Q /F Do", IMAGE, "0 0 60 60", IMAGE, "", true),
        ("restored-inner", "0 0 72 72", "", "/F Do", "q 0 0 1 1 re W n Q /I Do", "0 0 60 60", IMAGE, "", true),
        ("across-streams", "0 0 72 72", "", "q 0 0 72 72 re W n", IMAGE, "0 0 60 60", IMAGE, "Q /F Do", true),
        ("unknown-across-streams", "0 0 72 72", "", "0 0 72 72 re W n", IMAGE, "0 0 60 60", IMAGE, "/F Do", false),
        ("image-real-integral", "0 0 72 72", "", "/F Do", "q 36.0 0 0 36 18 18 cm /Im Do Q", "0 0 60 60", IMAGE, "", true),
        ("image-reflection", "0 0 72 72", "", "/F Do", "q -36 0 0 36 54 18 cm /Im Do Q", "0 0 60 60", IMAGE, "", true),
        ("image-shear", "0 0 72 72", "", "/F Do", "q 30 0 5 30 18 18 cm /Im Do Q", "0 0 60 60", IMAGE, "", true),
        ("rounded-image-real", "0 0 72 72", "", "/F Do", "q 36.000000000000000000000001 0 0 36 18 18 cm /Im Do Q", "0 0 60 60", IMAGE, "", false),
        ("cancelled-real", "0 0 72 72", "", "0.5 0 0 0.5 0 0 cm 2 0 0 2 0 0 cm /F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
        ("bad-matrix-item", "0 0 72 72", "/Matrix [1 0 0 1 0 0 /Extra]", "/F Do", IMAGE, "0 0 60 60", IMAGE, "", false),
    ];
    let directory = std::env::var("CHECK_FORM_CLIP_EVIDENCE_DIR").ok().map(std::path::PathBuf::from);
    if let Some(dir) = &directory {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut manifest = vec![];
    for (name, bbox, matrix, page, form, inner_bbox, inner, tail, eligible) in cases {
        let bytes = fixture(bbox, matrix, page, form, inner_bbox, inner, tail);
        let order = vectorcraft_pdf::print_order(&bytes).unwrap();
        let uses: Vec<_> = order.images.iter().flatten().collect();
        assert_eq!(uses.len(), 1, "{name}");
        assert!(uses[0].clip_context, "{name}");
        assert_eq!(uses[0].containing_form_clip.is_some(), eligible, "{name}");
        let bounded = matches!(name, "real-box" | "real-matrix" | "cancelled-real");
        assert_eq!(uses[0].form_clip_interior.is_some(), bounded, "{name}");
        let captured = eligible || bounded;
        let capture = crate::prepare::capture(&bytes, "form-clip-test".into(), "synthetic".into()).unwrap();
        assert_eq!(capture.raster_sources.len(), usize::from(captured), "{name}: {:?}", capture.diagnostics);
        assert!(!capture.diagnostics.is_empty(), "a boundary decision remains required");
        if let Some(dir) = &directory {
            std::fs::write(dir.join(format!("{name}.pdf")), &bytes).unwrap();
            std::fs::write(dir.join(format!("{name}-raw.json")), serde_json::to_vec(&order).unwrap()).unwrap();
            std::fs::write(dir.join(format!("{name}-capture.json")), serde_json::to_vec(&capture).unwrap()).unwrap();
        }
        manifest
            .push(json!({"name":name,"eligible":captured,"exact_eligible":eligible,"bounded_eligible":bounded,"pdf_sha256":crate::sha256(&bytes)}));
    }
    if let Some(dir) = &directory {
        std::fs::write(dir.join("cases.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }
}

#[test]
fn form_clip_q_q_restores_prior_image_and_source_digest_retains_clip() {
    let bytes = fixture("0 0 72 72", "", "/F Do q 36 0 0 36 18 18 cm /Im Do Q", IMAGE, "0 0 60 60", IMAGE, "");
    let order = vectorcraft_pdf::print_order(&bytes).unwrap();
    let images = &order.images[0];
    assert_eq!(images.len(), 2);
    assert!(images[0].clip_context && images[0].containing_form_clip.is_some());
    assert!(!images[1].clip_context && images[1].containing_form_clip.is_none());
    let raw = serde_json::to_value(&images[1]).unwrap();
    assert!(raw.get("containing_form_clip").is_none(), "unclipped identities remain unchanged");
    let capture = crate::prepare::capture(&bytes, "clip".into(), "synthetic".into()).unwrap();
    let source = &capture.raster_sources["page-0-image-0"];
    assert_eq!(source.mask.source_sha256, crate::digest(&images[0]).unwrap());
    assert_ne!(crate::digest(&images[0]).unwrap(), crate::digest(&images[1]).unwrap());
}

#[test]
fn malformed_graphics_restore_cannot_escape_a_form_clip() {
    for (page, form, inner) in [
        ("/F Do", "Q Q /I Do", IMAGE),
        ("0 0 72 72 re W n /F Do", "Q Q /I Do", IMAGE),
        ("/F Do", "/I Do", "Q Q q 36 0 0 36 18 18 cm /Im Do Q"),
        ("Q /F Do", IMAGE, IMAGE),
        ("q /F Do", IMAGE, IMAGE),
        ("/F Do", "q q 36 0 0 36 18 18 cm /Im Do Q", IMAGE),
    ] {
        let bytes = fixture("0 0 20 20", "", page, form, "0 0 72 72", inner, "");
        let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
        assert!(raw.issues.iter().any(|s| s.contains("graphics")), "{page} {form} {inner}");
        let capture = crate::prepare::capture(&bytes, "bad-save".into(), "synthetic".into()).unwrap();
        assert!(capture.raster_sources.is_empty());
    }
}

#[test]
fn clip_proof_survives_content_reader_windows_without_erasing_unknown_clips() {
    let padding = "q Q ".repeat(40_000);
    for unknown in [false, true] {
        let prefix = if unknown { "0 0 72 72 re W n " } else { "" };
        let page = format!("{prefix}{padding}/F Do");
        let bytes = fixture("0 0 72 72", "", &page, IMAGE, "0 0 60 60", IMAGE, "");
        let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
        assert_eq!(raw.images[0][0].containing_form_clip.is_some(), !unknown);
        let capture = crate::prepare::capture(&bytes, "windows".into(), "synthetic".into()).unwrap();
        assert_eq!(capture.raster_sources.len(), usize::from(!unknown));
    }
}

#[test]
#[ignore = "requires an explicitly configured independent Ghostscript binary"]
fn exact_form_transforms_and_nested_bounds_match_independent_direct_placements() {
    let Ok(binary) = std::env::var("CHECK_GS_BINARY") else { return };
    let base = std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    for (name, bbox, transform, paint, inner, direct) in [
        ("nested", "0 0 72 72", "", "/I Do", IMAGE, IMAGE),
        ("reflected", "0 0 72 72", "/Matrix [-1 0 0 1 72 0]", IMAGE, IMAGE, "q -36 0 0 36 54 18 cm /Im Do Q"),
        ("translated", "0 0 60 60", "/Matrix [1 0 0 1 1 2]", IMAGE, IMAGE, "q 36 0 0 36 19 20 cm /Im Do Q"),
        ("scaled", "0 0 36 36", "/Matrix [2 0 0 2 0 0]", "q 18 0 0 18 9 9 cm /Im Do Q", IMAGE, IMAGE),
        ("near-bound-cancellation", "1048500 0 1048572 72", "/Matrix [1 0 0 1 -1048500 0]", "q 36 0 0 36 1048518 18 cm /Im Do Q", IMAGE, IMAGE),
    ] {
        let expected = fixture("0 0 72 72", "", direct, IMAGE, "0 0 60 60", IMAGE, "");
        let actual = fixture(bbox, transform, "/F Do", paint, "0 0 60 60", inner, "");
        let raw = vectorcraft_pdf::print_order(&actual).unwrap();
        assert!(raw.images[0][0].containing_form_clip.is_some());
        for dpi in [72, 144] {
            let config = crate::independent::Configuration {
                binary: binary.clone().into(),
                binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                version: "10.07.1".into(),
                dpi,
                timeout_ms: 10000,
                max_output_bytes: 16 * 1024 * 1024,
                qualification_sha256: None,
            };
            let mut signatures = vec![];
            let mut renders = vec![];
            for (role, bytes) in [("direct", &expected), ("form", &actual)] {
                let dir = base.join(format!("form-transforms-{stamp}-{name}-{dpi}-{role}"));
                let render = crate::independent::render(&config, bytes, &dir).unwrap();
                assert!(!render.qualified);
                assert!(render.diagnostics.is_empty(), "{name}/{role}: {:?}", render.diagnostics);
                signatures.push(
                    render
                        .planes
                        .iter()
                        .map(|p| {
                            let pixels = image::open(&p.path).unwrap().to_luma8();
                            ((p.page, p.name.clone()), (p.width, p.height, crate::sha256(pixels.as_raw())))
                        })
                        .collect::<std::collections::BTreeMap<_, _>>(),
                );
                renders.push(render);
            }
            assert_eq!(signatures[0], signatures[1], "{name}/{dpi}");
            std::fs::write(
                base.join(format!("form-transforms-{stamp}-{name}-{dpi}.json")),
                serde_json::to_vec_pretty(
                    &json!({"engine":crate::ENGINE_VERSION,"name":name,"dpi":dpi,"samples_equal":true,"configuration":config,"renders":renders}),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }
}

#[path = "raster_prepare_decimal.rs"]
mod decimal;
