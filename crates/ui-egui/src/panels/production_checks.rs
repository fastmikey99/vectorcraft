//! Read-only findings presentation. All checker work and invalidation belongs to commands.
use crate::{VectorcraftApp, widgets};
use egui::Ui;
use serde_json::json;

pub fn show(app: &mut VectorcraftApp, ui: &mut Ui) {
    let Some(st) = app.session.active() else {
        widgets::dim_label(ui, "No document open.");
        return;
    };
    let intent = st.doc.trapping.clone();
    let has_selection = !st.selection.objects.is_empty();
    let state = st.production_checks.clone();
    let stale = state.is_stale(&st.doc);
    let mut mode = intent
        .as_ref()
        .map(|i| match i.mode {
            vectorcraft_doc::trapping::TrapMode::Trap => "trap",
            vectorcraft_doc::trapping::TrapMode::NoTrap => "no_trap",
            vectorcraft_doc::trapping::TrapMode::Mixed => "mixed",
        })
        .unwrap_or("");
    let old_mode = mode;
    egui::ComboBox::from_label("Colour trapping")
        .selected_text(match mode {
            "trap" => "Trap",
            "no_trap" => "No trap",
            "mixed" => "Mixed",
            _ => "Not supplied",
        })
        .show_ui(ui, |ui| {
            for (key, label) in [("", "Not supplied"), ("trap", "Trap"), ("no_trap", "No trap"), ("mixed", "Mixed")] {
                ui.selectable_value(&mut mode, key, label);
            }
        });
    if mode != old_mode {
        let value = if mode.is_empty() {
            serde_json::Value::Null
        } else {
            json!({"mode":mode,"exclusions":if mode=="mixed" {serde_json::Value::Null} else {json!([])},"objects":[]})
        };
        if let Err(e) = app.run("production.trapping", json!({"intent":value})) {
            app.ui.status = e;
        }
    }
    if ui.add_enabled(has_selection, egui::Button::new("Set selection as No trap")).clicked()
        && let Err(e) = app.run("production.trapping", json!({"selectedNoTrap":true}))
    {
        app.ui.status = e;
    }
    if let Some(intent) = intent.filter(|i| i.mode == vectorcraft_doc::trapping::TrapMode::Mixed) {
        widgets::dim_label(
            ui,
            &format!(
                "{} native No trap objects; {}",
                intent.objects.len(),
                intent.exclusions.map_or_else(|| "mixed-trap exclusions not known".into(), |r| format!("{} explicit regions", r.len()))
            ),
        );
    }
    ui.horizontal(|ui| {
        let (label, command) = if state.running() { ("Cancel checks", "production.checks.cancel") } else { ("Run checks", "production.checks.run") };
        if widgets::flat_button(ui, label, 100.0).clicked()
            && let Err(e) = app.run(command, json!({}))
        {
            app.ui.status = e;
        }
        if stale {
            widgets::dim_label(ui, "Document edited since this check");
        }
    });
    if state.running() {
        widgets::dim_label(ui, if state.cancelling() { "Cancelling…" } else { "Checking a document snapshot…" });
    }
    if let Some(error) = &state.error {
        ui.label(error);
    }
    let Some(report) = state.report else {
        widgets::dim_label(ui, "Choose the process recipe and Dark/Light in Reporter.");
        return;
    };
    ui.label(format!(
        "{:?} · {} / {} applicable checks evaluated",
        report.status,
        report.coverage.passed + report.coverage.failed,
        report.coverage.total - report.coverage.not_applicable
    ));
    ui.label(format!(
        "{} passes · {} needs fixing · {} cannot yet check · {} not applicable",
        report.coverage.passed, report.coverage.failed, report.coverage.incomplete, report.coverage.not_applicable
    ));
    widgets::dim_label(ui, "Checks describe the selected PDF and recipe. Unmeasured print questions are listed individually.");
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
        assert!(texts.iter().any(|s| s.contains("process recipe and Dark/Light")));
        assert!(texts.iter().any(|s| s == "Run checks"));
        assert!(texts.iter().any(|s| s == "Colour trapping"));
        assert!(texts.iter().any(|s| s == "Set selection as No trap"));
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
        assert!(texts.iter().any(|s| s.contains("1 needs fixing") && s.contains("1 cannot yet check")), "{texts:?}");
        assert!(texts.iter().any(|s| s.contains("Unmeasured print questions")), "{texts:?}");
    }
}
