//! Source continuity and independent plates for provably non-cropping forms.
use super::tests::{check_source, original, request_for_original};
use crate::Status;
use serde_json::json;

fn form(samples: &str, bbox: &str, prefix: &str, matrix: &str) -> Vec<u8> {
    let content = "/OC /Layer BDC /F Do EMC";
    let paint = format!("{prefix} q {matrix} cm /Im Do Q");
    crate::pdf::tests::objects_pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [7 0 R] /D << /Order [7 0 R] /ON [7 0 R] >> >> >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 144 144] /Resources << /Properties << /Layer 7 0 R >> /XObject << /Im 5 0 R /F 8 0 R >> >> /Contents 4 0 R >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        format!("<< /Type /XObject /Subtype /Image /Width 3 /Height 3 /BitsPerComponent 8 /ColorSpace /DeviceGray /SMask 6 0 R /Filter /ASCIIHexDecode /Length {} >>\nstream\n{samples}>\nendstream",samples.len()+1),
        "<< /Type /XObject /Subtype /Image /Width 3 /Height 3 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /ASCIIHexDecode /Length 19 >>\nstream\nffffffff00ffffffff>\nendstream".into(),
        "<< /Type /OCG /Name (TECH Output) >>".into(),
        format!("<< /Type /XObject /Subtype /Form /BBox [{bbox}] /Resources << /XObject << /Im 5 0 R >> >> /Length {} >>\nstream\n{paint}\nendstream",paint.len()),
    ])
}
const SAMPLES: &str = "0020406080a0c0e0ff";
const MATRIX: &str = "36 0 0 36 18 18";

#[test]
fn retained_form_containment_resolves_source_continuity_without_waiving_defects() {
    let source = original();
    for (name, bbox, prefix, samples, matrix, expected) in [
        ("form-contained", "0 0 72 72", "", SAMPLES, MATRIX, Status::Pass),
        (
            "form-export-box",
            "-32767 -32767 32767 32767",
            "q -32000 0 1 1 re W n Q",
            SAMPLES,
            "36.000000000000 0.000000000000 0.000000000000 36.000000000000 18.000000000000 18.000000000000",
            Status::Pass,
        ),
        ("form-tone-defect", "0 0 72 72", "", "1120406080a0c0e0ff", MATRIX, Status::Fail),
        ("form-shift-defect", "0 0 72 72", "", SAMPLES, "36 0 0 36 19 18", Status::Fail),
        ("form-cropped", "18 18 40 54", "", SAMPLES, MATRIX, Status::Incomplete),
        ("form-touching", "18 18 54 54", "", SAMPLES, MATRIX, Status::Incomplete),
        ("form-path-unknown", "0 0 72 72", "0 0 72 72 re W n", SAMPLES, MATRIX, Status::Incomplete),
        ("form-rounded-matrix", "0 0 72 72", "", SAMPLES, "36.0000000000000000001 0 0 36 18 18", Status::Incomplete),
    ] {
        let bytes = form(samples, bbox, prefix, matrix);
        let request = request_for_original(&source, &bytes, &[]);
        check_source(name, &request, &bytes, &source, expected);
        let report = crate::pdf::check(&request, &bytes).unwrap();
        assert_eq!(report.validity, crate::Validity::Provisional);
        assert_ne!(report.status, Status::Pass);
        assert_eq!(report.findings.iter().find(|f| f.obligation.key == "global/independent").unwrap().status, Status::Incomplete);
    }
}

#[test]
#[ignore = "requires an explicitly configured independent Ghostscript binary"]
fn form_containment_matches_independent_samples_and_rejects_changed_planes() {
    let Ok(binary) = std::env::var("CHECK_GS_BINARY") else { return };
    let base = std::env::var_os("CHECK_FEATURE_EVIDENCE_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    for dpi in [72, 144] {
        let mut original_samples = None;
        for (name, bytes, same) in [
            ("source", original(), true),
            ("contained", form(SAMPLES, "0 0 72 72", "", MATRIX), true),
            ("export-box", form(SAMPLES, "-32767 -32767 32767 32767", "q -32000 0 1 1 re W n Q", MATRIX), true),
            ("cropped", form(SAMPLES, "18 18 40 54", "", MATRIX), false),
        ] {
            let output = base.join(format!("form-containment-{stamp}-{name}-{dpi}"));
            let config = crate::independent::Configuration {
                binary: binary.clone().into(),
                binary_sha256: crate::sha256(&std::fs::read(&binary).unwrap()),
                version: "10.07.1".into(),
                dpi,
                timeout_ms: 10000,
                max_output_bytes: 16 * 1024 * 1024,
                qualification_sha256: None,
            };
            let mut render = crate::independent::render(&config, &bytes, &output).unwrap();
            assert!(!render.qualified);
            let signatures: std::collections::BTreeMap<_, _> = render
                .planes
                .iter()
                .map(|p| {
                    let pixels = image::open(&p.path).unwrap().to_luma8();
                    ((p.page, p.name.clone()), (p.width, p.height, crate::sha256(pixels.as_raw())))
                })
                .collect();
            if let Some(expected) = &original_samples {
                assert_eq!(&signatures == expected, same, "{name}/{dpi}");
            } else {
                original_samples = Some(signatures.clone());
            }
            let mut positive = None;
            let mut negative = None;
            let before = serde_json::to_value(&render).unwrap();
            if same {
                let p = crate::raster_images::compare(&bytes, &render, Some(0.6)).unwrap();
                assert_eq!(p.status, Status::Pass, "{name}/{dpi}: {p:?}");
                positive = Some(p);
                let plane = render.planes.iter_mut().find(|p| p.name == "Black").unwrap();
                let mut pixels = image::open(&plane.path).unwrap().to_luma8();
                assert_eq!(pixels.get_pixel(1, 1).0[0], 255);
                pixels.put_pixel(1, 1, image::Luma([0]));
                plane.path = output.join("changed-black.tif");
                pixels.save(&plane.path).unwrap();
                plane.sha256 = crate::sha256(&std::fs::read(&plane.path).unwrap());
                plane.minimum = 0;
                let n = crate::raster_images::compare(&bytes, &render, Some(0.6)).unwrap();
                assert_eq!(n.status, Status::Fail, "{name}/{dpi}: {n:?}");
                negative = Some(n);
            }
            std::fs::write(output.join("qualification.json"),serde_json::to_vec_pretty(&json!({"engine":crate::ENGINE_VERSION,"name":name,"dpi":dpi,"same_as_original_samples":same,"pdf_sha256":crate::sha256(&bytes),"positive":positive,"negative":negative,"render":before,"defect_render":render,"configuration":config})).unwrap()).unwrap();
        }
    }
}
