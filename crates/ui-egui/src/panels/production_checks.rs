//! Read-only findings presentation. All checker work and invalidation belongs to commands.
use crate::{VectorcraftApp, widgets};
use egui::Ui;
use serde_json::json;

pub fn show(app: &mut VectorcraftApp, ui: &mut Ui) {
    let Some(st) = app.session.active() else {
        widgets::dim_label(ui, "No document open.");
        return;
    };
    let state = st.production_checks.clone();
    let validity = state.validity(&st.doc);
    ui.horizontal(|ui| {
        let (label, command) = if state.running() { ("Cancel checks", "production.checks.cancel") } else { ("Run checks", "production.checks.run") };
        if widgets::flat_button(ui, label, 100.0).clicked()
            && let Err(e) = app.run(command, json!({}))
        {
            app.ui.status = e;
        }
        widgets::dim_label(ui, &format!("{validity:?}"));
    });
    if state.running() {
        widgets::dim_label(ui, if state.cancelling() { "Cancelling…" } else { "Checking a document snapshot…" });
    }
    if let Some(error) = &state.error {
        ui.label(error);
    }
    let Some(report) = state.report else {
        widgets::dim_label(ui, "Choose a recipe and preparation baseline in Reporter to configure checks.");
        return;
    };
    ui.label(format!(
        "{:?} · {} / {} applicable checks evaluated",
        report.status,
        report.coverage.passed + report.coverage.failed,
        report.coverage.total - report.coverage.not_applicable
    ));
    ui.label(format!(
        "{} passed · {} failed · {} incomplete · {} not applicable",
        report.coverage.passed, report.coverage.failed, report.coverage.incomplete, report.coverage.not_applicable
    ));
    widgets::dim_label(ui, "Local findings are provisional. Final Pass requires verification of the saved PDF.");
    let mut groups = std::collections::BTreeMap::<String, Vec<_>>::new();
    for finding in &report.findings {
        groups.entry(finding.obligation.area.clone()).or_default().push(finding);
    }
    for (area, findings) in groups {
        ui.collapsing(area, |ui| {
            for f in findings {
                let text = format!("{:?} · {}", f.status, f.obligation.label);
                if ui.selectable_label(state.selected.as_deref() == Some(&f.obligation.key), text).clicked()
                    && let Err(e) = app.run("production.checks.select", json!({"key":f.obligation.key}))
                {
                    app.ui.status = e;
                }
                if state.selected.as_deref() == Some(&f.obligation.key) {
                    ui.label(&f.reason);
                    ui.label(format!("Owner: {:?}", f.obligation.owner));
                    if let Some(plate) = &f.obligation.plate {
                        ui.label(format!("Plate: {plate}"));
                    }
                    ui.collapsing("Expected / observed / evidence", |ui| {
                        ui.monospace(format!("Expected: {}\nObserved: {}", f.obligation.expected, f.observed));
                        for e in &f.evidence {
                            ui.label(format!("{} · {}", e.producer, e.detail));
                        }
                    });
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectorcraft_engine::Session;
    fn frame(app: &mut VectorcraftApp) -> Vec<String> {
        fn text(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().into()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| text(s, out)),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| show(app, ui));
        output.textures_delta.clear();
        let mut texts = vec![];
        for shape in output.shapes {
            text(&shape.shape, &mut texts);
        }
        texts
    }
    #[test]
    fn no_document_and_missing_configuration_have_clear_preparation_path() {
        let mut app = VectorcraftApp::new(Session::new(), Default::default());
        assert!(frame(&mut app).iter().any(|s| s == "No document open."));
        app.session.execute("file.new", &json!({"width":100,"height":100})).unwrap();
        let texts = frame(&mut app);
        assert!(texts.iter().any(|s| s.contains("preparation baseline")));
        assert!(texts.iter().any(|s| s == "Run checks"));
    }
    #[test]
    fn failure_summary_keeps_unchecked_coverage_visible() {
        let mut app = VectorcraftApp::new(Session::new(), Default::default());
        app.session.execute("file.new", &json!({"width":100,"height":100})).unwrap();
        let identity = json!({"recipe_sha256":"r","baseline_sha256":"b","original_sha256":"o","document_sha256":"d","pdf_sha256":"p","job_revision":"j","assignments_sha256":"a","measurement_sha256":"m","engine_version":"test"});
        let finding = |key: &str, status: &str| json!({"obligation":{"key":key,"area":"Final PDF","label":key,"rule":null,"file_link":null,"plate":null,"owner":"engineering","expected":null,"conditional":false},"status":status,"reason":"Synthetic panel test","observed":null,"evidence":[],"location":null,"uncertainty_mm":null});
        app.session.doc_mut().unwrap().production_checks.report = Some(
            serde_json::from_value(json!({
                "contract_version":1,"run_id":"synthetic-panel-test","identity":identity,"status":"fail","validity":"provisional",
                "coverage":{"total":2,"passed":0,"failed":1,"incomplete":1,"not_applicable":0},
                "findings":[finding("global/export","fail"),finding("global/independent","incomplete")]
            }))
            .unwrap(),
        );
        let texts = frame(&mut app);
        assert!(texts.iter().any(|s| s.contains("1 / 2 applicable checks evaluated")), "{texts:?}");
        assert!(texts.iter().any(|s| s.contains("1 failed") && s.contains("1 incomplete")), "{texts:?}");
        assert!(texts.iter().any(|s| s.contains("Local findings are provisional")), "{texts:?}");
    }
}
