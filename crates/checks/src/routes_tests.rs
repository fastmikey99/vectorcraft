use super::*;
use crate::{Report, Request, Status};
use std::collections::BTreeMap;

// Original two-ink, two-group PDF, written from public ISO 32000 syntax.
fn pdf(labels: [&str; 2], inks: [&str; 2], reverse: bool, overprint: bool, first_tint: u8) -> Vec<u8> {
    let first = format!("q /OC /A BDC /GA gs /A cs {first_tint} scn 10 10 10 10 re f EMC Q");
    let second = "q /OC /B BDC /GB gs /B cs 1 scn 30 10 10 10 re f EMC Q";
    let stream = if reverse { format!("{second}\n{first}") } else { format!("{first}\n{second}") };
    pdf_stream(labels, inks, &stream, overprint)
}
fn pdf_stream(labels: [&str; 2], inks: [&str; 2], stream: &str, overprint: bool) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [6 0 R 7 0 R] /D << /Order [6 0 R 7 0 R] /ON [6 0 R 7 0 R] >> >> >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /A 6 0 R /B 7 0 R >> /ColorSpace << /A [/Separation /{} /DeviceCMYK 5 0 R] /B [/Separation /{} /DeviceCMYK 5 0 R] >> /ExtGState << /GA 8 0 R /GB 9 0 R >> >> /Contents 4 0 R >>",
            inks[0], inks[1]
        ),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>".into(),
        format!("<< /Type /OCG /Name ({}) >>", labels[0]),
        format!("<< /Type /OCG /Name ({}) >>", labels[1]),
        format!("<< /Type /ExtGState /OP {overprint} /op {overprint} /OPM 1 >>"),
        "<< /Type /ExtGState /OP true /op true /OPM 1 >>".into(),
    ];
    let mut data = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(data.len());
        data.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = data.len();
    data.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        data.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    data
}
fn request() -> Request {
    let mut r = crate::tests::request();
    r.recipe.required_screens = Some(vec!["First".into()]);
    let first = &mut r.recipe.file_links[0];
    first.pdf_layer = "White Underprint-1".into();
    first.separation_name = Some("First".into());
    let mut upper = first.clone();
    upper.key = "upper".into();
    upper.pdf_layer = "White Underprint-5".into();
    upper.separation_name = Some("Second".into());
    upper.destination = "Indigo".into();
    upper.layer_order = 2;
    r.recipe.file_links.push(upper);
    r
}
fn check(mut r: Request, bytes: &[u8]) -> Report {
    r.identity.pdf_sha256 = crate::sha256(bytes);
    r.identity.recipe_sha256 = crate::digest(&r.recipe).unwrap();
    r.input_class = Some(crate::preparation::Fact {
        value: serde_json::json!("individual_separation"),
        provenance: crate::preparation::Provenance::File {
            artifact_sha256: r.identity.pdf_sha256.clone(),
            locator: "Original synthetic two-output separation".into(),
        },
    });
    r.identity.assignments_sha256 = r.assignments_digest().unwrap();
    crate::pdf::check(&r, bytes).unwrap()
}
fn status(r: &Report, key: &str) -> Status {
    r.findings.iter().find(|f| f.obligation.key == key).unwrap().status
}
#[test]
fn only_the_documented_numbered_family_is_a_fingerprint() {
    for label in ["White Underprint", "White Underprint-1", "White Underprint-005", "White Underprint-99999999999999999999999999"] {
        assert!(layer_matches("White Underprint-5", label));
    }
    for label in ["White Underprint-", "White Underprint-X", "White Underprint-5 ", "white underprint-5", "Artwork"] {
        assert!(!layer_matches("White Underprint-5", label));
    }
    assert!(!layer_matches("Layer-1", "Layer-2"));
    assert!(!layer_matches("Auto Trap-1", "Auto Trap-2"));
}
#[test]
fn renumbered_or_unnumbered_underprints_retain_all_finding_statuses() {
    let control = check(request(), &pdf(["White Underprint-1", "White Underprint-5"], ["First", "Second"], false, true, 1));
    let findings = |r: &Report| r.findings.iter().map(|f| (f.obligation.key.clone(), f.status)).collect::<BTreeMap<_, _>>();
    for labels in
        [["White Underprint-5", "White Underprint-1"], ["White Underprint-17", "White Underprint-2"], ["White Underprint", "White Underprint-005"]]
    {
        let report = check(request(), &pdf(labels, ["First", "Second"], false, true, 1));
        assert_eq!(findings(&report), findings(&control));
        for key in ["file/output/identity", "file/output/layer", "file/output/order", "file/upper/order", "observed/routing"] {
            assert_eq!(status(&report, key), Status::Pass, "{key}");
        }
    }
}
#[test]
fn renumbering_does_not_waive_wrong_ink_or_wrong_layer_family() {
    for (labels, inks) in
        [(["White Underprint-17", "White Underprint-2"], ["Wrong", "Second"]), (["Artwork", "White Underprint-2"], ["First", "Second"])]
    {
        let report = check(request(), &pdf(labels, inks, false, true, 1));
        assert_eq!(status(&report, "observed/routing"), Status::Fail);
        assert_eq!(status(&report, "file/output/order"), Status::Incomplete);
        assert_eq!(status(&report, "file/upper/order"), Status::Incomplete);
    }
}
#[test]
fn actual_order_and_overprint_remain_independent_of_the_number() {
    let bytes = pdf(["White Underprint-17", "White Underprint-2"], ["First", "Second"], true, true, 1);
    let report = check(request(), &bytes);
    assert_eq!(status(&report, "file/output/order"), Status::Fail);
    assert_eq!(status(&report, "file/upper/order"), Status::Fail);
    let report = check(request(), &pdf(["White Underprint-17", "White Underprint-2"], ["First", "Second"], false, false, 1));
    assert_eq!(status(&report, "file/output/overprint"), Status::Fail);
}
#[test]
fn conflicting_destination_roles_cannot_be_resolved_by_a_layer_number() {
    let mut r = request();
    r.recipe.file_links[1].separation_name = Some("First".into());
    let report = check(r, &pdf(["White Underprint-1", "White Underprint-5"], ["First", "First"], false, true, 1));
    for key in ["observed/routing", "file/output/identity", "file/output/order", "file/upper/order", "observed/file/output/order"] {
        assert_eq!(status(&report, key), Status::Incomplete, "{key}");
    }
}
#[test]
fn zero_tint_cannot_supply_missing_output_order() {
    let report = check(request(), &pdf(["White Underprint-17", "White Underprint-2"], ["First", "Second"], false, true, 0));
    assert_eq!(status(&report, "file/output/order"), Status::Incomplete);
    assert_eq!(status(&report, "file/upper/order"), Status::Incomplete);
    assert_eq!(status(&report, "observed/file/output/order"), Status::Incomplete);
    assert_eq!(status(&report, "observed/file/upper/order"), Status::Incomplete);
}

#[test]
fn all_conflicting_role_fields_refuse_hint_based_attribution() {
    for conflict in ["source", "overprint", "order"] {
        let mut r = request();
        let mut other = r.recipe.file_links[0].clone();
        other.key = "conflicting".into();
        other.pdf_layer = "White Underprint-999".into();
        match conflict {
            "source" => {
                other.name_mode = "source".into();
                other.separation_name = None;
            }
            "overprint" => other.overprint = !other.overprint,
            _ => other.layer_order += 1,
        }
        r.recipe.file_links.push(other);
        let report = check(r, &pdf(["White Underprint-17", "White Underprint-2"], ["First", "Second"], false, true, 1));
        for key in [
            "file/output/identity",
            "file/output/order",
            "observed/file/output/paint",
            "observed/routing",
            "file/upper/order",
            "observed/file/upper/order",
        ] {
            assert_eq!(status(&report, key), Status::Incomplete, "{conflict}: {key}");
        }
    }
}
#[test]
fn stray_hint_family_ink_is_not_blamed_on_unrelated_output() {
    let bytes = pdf(["White Underprint-17", "White Underprint-2"], ["Wrong", "Second"], false, true, 1);
    let report = check(request(), &bytes);
    assert_eq!(status(&report, "observed/routing"), Status::Fail);
    assert_eq!(status(&report, "observed/file/upper/paint"), Status::Pass);
    assert_eq!(status(&report, "observed/file/output/paint"), Status::Incomplete);
    let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
    crate::ordering::try_reconcile(&bytes, &mut census).unwrap();
    let r = request(); // Input classification deliberately absent.
    let mut report = crate::begin(&r).unwrap();
    let program = crate::program::Program { outputs: BTreeMap::from([("output".into(), vec![]), ("upper".into(), vec![])]), ..Default::default() };
    crate::paint_fidelity::check(&r, &program, &census, &mut report).unwrap();
    assert_eq!(status(&report, "global/colour"), Status::Fail);
    assert_eq!(status(&report, "rule/arbitrary/paint"), Status::Incomplete);
}

#[test]
fn exact_layer_conflicting_roles_also_refuse_unproved_attribution() {
    let mut r = request();
    for link in &mut r.recipe.file_links {
        link.pdf_layer = "Exact Layer".into();
        link.separation_name = Some("First".into());
    }
    let report = check(r, &pdf(["Exact Layer", "Exact Layer"], ["First", "First"], false, true, 1));
    for key in ["file/output/identity", "file/output/order", "observed/routing"] {
        assert_eq!(status(&report, key), Status::Incomplete);
    }
}

#[test]
fn partially_ambiguous_counterpart_cannot_prove_order() {
    let mut r = request();
    let mut ambiguous_exact = r.recipe.file_links[0].clone();
    ambiguous_exact.key = "ambiguous-exact".into();
    ambiguous_exact.layer_order = 2;
    r.recipe.file_links[0].name_mode = "source".into();
    r.recipe.file_links[0].separation_name = None;
    r.recipe.file_links[1].pdf_layer = "Artwork".into();
    r.recipe.file_links.push(ambiguous_exact);
    let bytes = pdf_stream(
        ["White Underprint-17", "Artwork"],
        ["First", "Second"],
        "q /OC /A BDC /GA gs 0 g 10 10 10 10 re f EMC Q\nq /OC /B BDC /GB gs /B cs 1 scn 30 10 10 10 re f EMC Q\nq /OC /A BDC /GA gs /A cs 1 scn 50 10 10 10 re f EMC Q",
        true,
    );
    let report = check(r, &bytes);
    for key in ["file/upper/order", "observed/file/upper/order"] {
        assert_eq!(status(&report, key), Status::Incomplete, "{key}");
    }
}
