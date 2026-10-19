//! Bounded decimal containment: retained source proof and conservative refusals.
use super::*;

#[test]
fn decimal_forms_capture_only_proven_inner_rectangles() {
    let cases = [
        ("fractional-box", "0.25 0.25 71.75 71.75", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, true),
        ("fractional-form-translation", "0 0 70 70", "/Matrix [1 0 0 1 .125 .25]", "/F Do", IMAGE, "0 0 60 60", IMAGE, true),
        ("fractional-form-scale", "0 0 56 56", "/Matrix [1.25 0 0 1.25 .125 .25]", "/F Do", IMAGE, "0 0 60 60", IMAGE, true),
        ("fractional-form-reflection", "0 0 70 70", "/Matrix [-1 0 0 1 70.125 .25]", "/F Do", IMAGE, "0 0 60 60", IMAGE, true),
        ("fractional-image", "0 0 72 72", "", "/F Do", "q 35.5 0 0 35.5 18.125 18.25 cm /Im Do Q", "0 0 60 60", IMAGE, true),
        ("fractional-image-shear", "0 0 72 72", "", "/F Do", "q 30.5 .25 5.125 30.5 18.125 18.25 cm /Im Do Q", "0 0 60 60", IMAGE, true),
        ("decimal-cancellation", "0 0 72 72", "", "0.5 0 0 .5 0 0 cm 2 0 0 2 0 0 cm /F Do", IMAGE, "0 0 60 60", IMAGE, true),
        ("nested-decimals", ".25 .25 71.75 71.75", "", "/F Do", "/I Do", "1.25 1.25 60.5 60.5", IMAGE, true),
        ("restored-decimal-path", ".25 .25 71.75 71.75", "", "q 0 0 1 1 re W n Q /F Do", IMAGE, "0 0 60 60", IMAGE, true),
        ("decimal-comments", "0.25 % comment\n .25 71.75 71.75", "/Matrix [1 0 % matrix\n0 1 .125 .25]", "/F Do", IMAGE, "0 0 60 60", IMAGE, true),
        ("decimal-crop", "18.25 18.25 40.25 54.25", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, false),
        ("decimal-touch", "18.25 18.25 54.25 54.25", "", "/F Do", "q 36 0 0 36 18.25 18.25 cm /Im Do Q", "0 0 60 60", IMAGE, false),
        (
            "insufficient-inner-margin",
            "18.125 18.125 54.125 54.125",
            "",
            "/F Do",
            "q 35.5 0 0 35.5 18.25 18.25 cm /Im Do Q",
            "0 0 60 60",
            IMAGE,
            false,
        ),
        ("unknown-outer-path", ".25 .25 71.75 71.75", "", "0 0 72 72 re W n /F Do", IMAGE, "0 0 60 60", IMAGE, false),
        ("unknown-inner-path", ".25 .25 71.75 71.75", "", "/F Do", "0 0 72 72 re W n /I Do", "0.25 0.25 60.5 60.5", IMAGE, false),
        ("nested-decimal-crop", ".25 .25 71.75 71.75", "", "/F Do", "/I Do", "0.25 0.25 20.5 60.5", IMAGE, false),
        ("decimal-form-shear", "0 0 70 70", "/Matrix [1 0 .125 1 0 0]", "/F Do", IMAGE, "0 0 60 60", IMAGE, false),
        ("decimal-array-tail", ".25 .25 71.75 71.75 999 0 R", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, false),
        ("decimal-matrix-tail", "0 0 70 70", "/Matrix [1 0 0 1 .125 .25 999 0 R]", "/F Do", IMAGE, "0 0 60 60", IMAGE, false),
        ("excess-decimal-precision", "0 0 72.000000000000000001 72", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, false),
        ("decimal-matrix-overflow", "0 0 72 72", "/Matrix [1048576.5 0 0 1 0 0]", "/F Do", IMAGE, "0 0 60 60", IMAGE, false),
        ("uncertain-small-determinant", "0 0 72 72", "", "/F Do", "q 30 30 30 30.000000000000001 18 18 cm /Im Do Q", "0 0 60 60", IMAGE, false),
        ("decimal-zero-width", "0 0 0.0 72", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, false),
        ("decimal-reversed-box", "71.75 .25 .25 71.75", "", "/F Do", IMAGE, "0 0 60 60", IMAGE, false),
    ];
    let dir = std::env::var_os("CHECK_DECIMAL_CLIP_EVIDENCE_DIR").map(std::path::PathBuf::from);
    if let Some(dir) = &dir {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut manifest = vec![];
    for (name, bbox, matrix, page, form, inner_bbox, inner, eligible) in cases {
        let bytes = fixture(bbox, matrix, page, form, inner_bbox, inner, "");
        let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
        let image = &raw.images[0][0];
        assert!(image.clip_context);
        assert!(image.containing_form_clip.is_none(), "{name}");
        assert_eq!(image.form_clip_interior.is_some(), eligible, "{name}: {image:?}");
        let capture = crate::prepare::capture(&bytes, "decimal-clip-test".into(), "synthetic".into()).unwrap();
        assert_eq!(capture.raster_sources.len(), usize::from(eligible), "{name}: {:?}", capture.diagnostics);
        if eligible {
            assert_eq!(capture.raster_sources["page-0-image-0"].mask.source_sha256, crate::digest(image).unwrap());
        }
        if let Some(dir) = &dir {
            std::fs::write(dir.join(format!("{name}.pdf")), &bytes).unwrap();
            std::fs::write(dir.join(format!("{name}-raw.json")), serde_json::to_vec(&raw).unwrap()).unwrap();
            std::fs::write(dir.join(format!("{name}-capture.json")), serde_json::to_vec(&capture).unwrap()).unwrap();
        }
        manifest.push(json!({"name":name,"eligible":eligible,"pdf_sha256":crate::sha256(&bytes)}));
    }
    if let Some(dir) = &dir {
        std::fs::write(dir.join("cases.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }
}

#[test]
fn decimal_proof_restores_state_without_changing_unclipped_identities() {
    let bytes = fixture(".25 .25 71.75 71.75", "", "/F Do q 36 0 0 36 18 18 cm /Im Do Q", IMAGE, "0 0 60 60", IMAGE, "");
    let raw = vectorcraft_pdf::print_order(&bytes).unwrap();
    assert!(raw.images[0][0].form_clip_interior.is_some());
    let direct = &raw.images[0][1];
    assert!(!direct.clip_context && direct.form_clip_interior.is_none() && direct.containing_form_clip.is_none());
    let value = serde_json::to_value(direct).unwrap();
    assert!(value.get("form_clip_interior").is_none());
}

#[test]
#[ignore = "requires an explicitly configured independent Ghostscript binary"]
fn decimal_form_transforms_match_independent_direct_samples() {
    let Ok(binary) = std::env::var("CHECK_GS_BINARY") else { return };
    let base = std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    for (name, bbox, matrix, paint, inner, direct, same) in [
        ("fractional-bounds", ".25 .25 71.75 71.75", "", IMAGE, IMAGE, IMAGE, true),
        ("translated", "0 0 70 70", "/Matrix [1 0 0 1 .125 .25]", IMAGE, IMAGE, "q 36 0 0 36 18.125 18.25 cm /Im Do Q", true),
        ("scaled", "0 0 56 56", "/Matrix [1.25 0 0 1.25 .125 .25]", IMAGE, IMAGE, "q 45 0 0 45 22.625 22.75 cm /Im Do Q", true),
        ("reflected", "0 0 70 70", "/Matrix [-1 0 0 1 70.125 .25]", IMAGE, IMAGE, "q -36 0 0 36 52.125 18.25 cm /Im Do Q", true),
        (
            "image-shear",
            ".25 .25 71.75 71.75",
            "",
            "q 30.5 .25 5.125 30.5 18.125 18.25 cm /Im Do Q",
            IMAGE,
            "q 30.5 .25 5.125 30.5 18.125 18.25 cm /Im Do Q",
            true,
        ),
        ("nested", ".25 .25 71.75 71.75", "", "/I Do", IMAGE, IMAGE, true),
        ("cropped", "18.25 18.25 40.25 54.25", "", IMAGE, IMAGE, IMAGE, false),
        ("shift-defect", ".25 .25 71.75 71.75", "", "q 36 0 0 36 19.25 18.25 cm /Im Do Q", IMAGE, IMAGE, false),
    ] {
        let direct = fixture("0 0 72 72", "", direct, IMAGE, "0 0 60 60", IMAGE, "");
        let form = fixture(bbox, matrix, "/F Do", paint, "1.25 1.25 60.5 60.5", inner, "");
        let raw = vectorcraft_pdf::print_order(&form).unwrap();
        assert_eq!(raw.images[0][0].form_clip_interior.is_some(), name != "cropped");
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
            for (role, bytes) in [("direct", &direct), ("form", &form)] {
                let directory = base.join(format!("decimal-form-{stamp}-{name}-{dpi}-{role}"));
                let render = crate::independent::render(&config, bytes, &directory).unwrap();
                assert!(!render.qualified && render.diagnostics.is_empty(), "{name}/{role}: {:?}", render.diagnostics);
                signatures.push(
                    render
                        .planes
                        .iter()
                        .map(|p| {
                            let image = image::open(&p.path).unwrap();
                            assert_eq!(image.color(), image::ColorType::L8);
                            ((p.page, p.name.clone()), (p.width, p.height, crate::sha256(image.as_bytes())))
                        })
                        .collect::<std::collections::BTreeMap<_, _>>(),
                );
                renders.push(render);
            }
            assert_eq!(signatures[0] == signatures[1], same, "{name}/{dpi}");
            std::fs::write(base.join(format!("decimal-form-{stamp}-{name}-{dpi}.json")),serde_json::to_vec_pretty(&json!({"engine":crate::ENGINE_VERSION,"name":name,"dpi":dpi,"samples_equal":same,"configuration":config,"renders":renders,"pdf_sha256":{"direct":crate::sha256(&direct),"form":crate::sha256(&form)}})).unwrap()).unwrap();
        }
    }
}
