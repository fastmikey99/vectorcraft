//! ICC settings are process-wide: keep this synthetic PAL regression in its own test binary.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use serde_json::json;
use vectorcraft_color::{Color, cms};
use vectorcraft_engine::Session;
use vectorcraft_testkit::pdf::{PdfPage, pdf};

fn action(kind: &str, params: &str) -> String {
    format!("<ActionListItem Type=\"{kind}\"><Parameters>{params}</Parameters></ActionListItem>")
}

#[test]
fn local_profile_description_drives_conversion_and_missing_profiles_keep_running() {
    let root = std::env::temp_dir().join(format!("vectorcraft-pal-profile-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let folder = root.join("profiles");
    std::fs::create_dir_all(&folder).unwrap();
    // Generate the project's own generic output profile, then give its description a unique
    // same-length UTF-16 name. No external profile or customer fixture is needed.
    let mut bytes = cms::icc_bytes(cms::GENERIC_CMYK).unwrap().to_vec();
    let name = "Synthetic PAL CMYK (Test Condition)";
    let name = format!("{name:width$}", width = cms::GENERIC_CMYK.len());
    let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<_>>();
    let old = utf16(cms::GENERIC_CMYK);
    let start = bytes.windows(old.len()).position(|b| b == old).unwrap();
    bytes[start..start + old.len()].copy_from_slice(&utf16(&name));
    std::fs::write(folder.join("unrelated-filename.ICM"), bytes).unwrap();
    std::fs::write(folder.join("broken.icc"), b"not an ICC").unwrap();
    std::fs::write(folder.join("rgb.icc"), &*cms::icc_bytes(cms::SRGB).unwrap()).unwrap();
    let input = root.join("input.pdf");
    std::fs::write(&input, pdf(&[PdfPage::new(100.0, 100.0, "0.8 0.3 0.4 rg 10 10 20 20 re f 0.1 0.2 0.3 0.4 k 40 40 10 10 re f")], None)).unwrap();
    let run = |s: &mut Session, body: String| {
        std::fs::write(root.join("main.pal"), format!("<ActionList Version=\"2.0\">{body}</ActionList>")).unwrap();
        s.execute("sep.runActionList", &json!({"root":root,"profiles":folder,"lists":["main"],"input":input,"output":root.join("out.pdf")})).unwrap()
    };
    let assign = |severity: &str, name: &str| {
        action("AssignDocumentProfile", &format!("<Profile>{name}</Profile><MessageSeverity>{severity}</MessageSeverity>"))
    };
    let mut s = Session::new();
    s.execute("edit.colorSettings", &json!({"intent":"relative","bpc":true})).unwrap();
    // Omit the parenthetical qualifier and vary spaces/case: match the description, not filename.
    let result = run(&mut s, assign("Error", "synthetic-pal cmyk"));
    assert_eq!(result["status"], "completed", "{result}");
    assert!(result["messages"].as_array().unwrap().is_empty());
    let registered = s.doc().unwrap().doc.color_profiles.cmyk.clone().unwrap();
    assert_eq!(registered.trim(), name.trim());
    let settings = cms::active_settings();
    assert_eq!(settings.intent, cms::Intent::RelativeColorimetric);
    assert!(settings.bpc);
    let converter = cms::Cms::new(&settings).unwrap();
    let want = converter.rgb_into_cmyk(&converter, [0.8, 0.3, 0.4], settings.intent).unwrap();
    assert!(want.iter().zip(Color::rgb(0.8, 0.3, 0.4).to_cmyk()).any(|(a, b)| (a - b).abs() > 0.01));
    // Public PDF content (not editing data) must have the profile-converted RGB and exact CMYK.
    s.execute("document.open", &json!({"path":root.join("out.pdf"),"printProduction":true})).unwrap();
    let mut colors = Vec::new();
    s.doc().unwrap().doc.visit_paints(&mut |p| {
        if let Some(c) = p.color() {
            colors.push(c.to_cmyk());
        }
    });
    let has = |want: [f32; 4]| colors.iter().any(|got| got.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.0001));
    assert!(has(want), "{colors:?} != {want:?}");
    assert!(has([0.1, 0.2, 0.3, 0.4]));
    let create = action(
        "CreateSeparation",
        "<SeparationName>Synthetic ink</SeparationName><PreviewRed>0.8</PreviewRed><PreviewGreen>0.3</PreviewGreen><PreviewBlue>0.4</PreviewBlue>",
    );
    // Delete an unused ink: the RGB paint must not be separated by the legacy formula.
    let result = run(&mut s, assign("Error", "synthetic-pal cmyk") + &create + &action("DeleteInk", ""));
    assert_eq!(result["status"], "completed");
    let mut colors = Vec::new();
    s.doc().unwrap().doc.visit_paints(&mut |p| {
        if let Some(c) = p.color() {
            colors.push(c.to_cmyk());
        }
    });
    assert!(colors.iter().any(|got| got.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.0001)), "{colors:?}");
    // PDF/preview RGB alternate, at 50% tint, uses the assigned profile exactly once.
    let result = run(
        &mut s,
        assign("Error", "synthetic-pal cmyk")
            + &action("SelectAllObjects", "")
            + &create
            + &action("SetFillFlatPaint", "<Percentage>50</Percentage>")
            + &action("ConvertToCMYK", "<Option>UseFromPDF</Option>"),
    );
    assert_eq!(result["status"], "completed");
    let doc = s.doc().unwrap();
    assert_eq!(doc.selection.objects.len(), 2);
    for id in &doc.selection.objects {
        let paint = doc.doc.node(*id).unwrap().appearance.fill_paint();
        let c = paint.color().unwrap();
        assert!(c.to_cmyk().iter().zip(want).all(|(a, b)| (a - b * 0.5).abs() < 0.0001), "{c:?}");
        assert!(matches!(paint, vectorcraft_color::Paint::Solid { swatch: None, .. }));
    }
    // The PAL supplies no intent/BPC: honor the existing color settings, including BPC off.
    s.execute("edit.colorSettings", &json!({"intent":"absolute","bpc":false})).unwrap();
    let result = run(&mut s, assign("Error", "synthetic-pal cmyk"));
    assert_eq!(result["status"], "completed");
    assert_eq!(cms::active_settings().intent, cms::Intent::AbsoluteColorimetric);
    assert!(!cms::active_settings().bpc);
    // An ambiguous configured directory must not reuse a previously registered match.
    std::fs::copy(folder.join("unrelated-filename.ICM"), folder.join("duplicate.icc")).unwrap();
    let result = run(&mut s, assign("Error", "synthetic-pal cmyk"));
    assert_eq!(result["status"], "stopped");
    assert!(result["stoppedAt"]["reason"].as_str().unwrap().contains("ambiguous"));
    std::fs::remove_file(folder.join("duplicate.icc")).unwrap();
    // Missing folder/profile keeps Info severity nonfatal and tells the operator how to supply it.
    let missing = run(&mut s, assign("Info", "Absent Test Profile"));
    assert_eq!(missing["status"], "completed");
    assert!(missing["messages"][0].as_str().unwrap().contains("VECTORCRAFT_PROFILES"));
    let missing = run(&mut s, assign("Error", "Absent Test Profile"));
    assert_eq!(missing["status"], "stopped");
    assert!(missing["stoppedAt"]["reason"].as_str().unwrap().contains("VECTORCRAFT_PROFILES"));
    assert!(s.execute("sep.runActionList", &json!({"profiles":42})).unwrap_err().to_string().contains("profiles"));
    std::fs::remove_dir_all(root).unwrap();
}
