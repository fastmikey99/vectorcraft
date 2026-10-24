//! A single-document editor (`Services::single_document`, the web build's `?host=parent`): only the
//! file the embedding page hands over is open, and nothing the person does opens another.

use std::sync::Arc;

use serde_json::json;

use vectorcraft_doc::Unit;
use vectorcraft_engine::units::Measure;

use crate::{HostInbox, HostOpen, Inbox, VectorcraftApp, menus};

const SQUARE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect x="10" y="10" width="20" height="20"/></svg>"#;
const CIRCLE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50"><circle cx="25" cy="25" r="10"/></svg>"#;

fn single() -> (VectorcraftApp, Inbox, HostInbox) {
    let (inbox, host): (Inbox, HostInbox) = (Arc::default(), Arc::default());
    let services = crate::Services { single_document: true, inbox: Some(inbox.clone()), host_inbox: Some(host.clone()), ..Default::default() };
    (VectorcraftApp::new(vectorcraft_engine::Session::new(), services), inbox, host)
}

fn hand(app: &mut VectorcraftApp, inbox: &HostInbox, name: &str, bytes: &[u8]) {
    inbox.lock().unwrap().push(HostOpen {
        name: name.to_string(),
        bytes: bytes.to_vec(),
        print_production: false,
        separations: None,
        raster_effects_ppi: None,
        units: None,
        stroke_units: None,
    });
    app.drain_inbox();
}

fn titles(app: &VectorcraftApp) -> Vec<String> {
    app.session.documents().iter().map(|d| d.title()).collect()
}

#[test]
fn a_file_from_the_page_takes_the_open_documents_place() {
    let (mut app, _, host) = single();
    hand(&mut app, &host, "first.svg", SQUARE);
    assert_eq!(titles(&app), ["first.svg"]);
    // Unsaved changes don't stop it: the page decides (it hears `vectorcraft:dirty`).
    app.run("select.all", json!({})).unwrap();
    app.run("object.move", json!({ "dx": 5, "dy": 0 })).unwrap();
    assert!(app.session.active().unwrap().is_dirty());
    hand(&mut app, &host, "second.svg", CIRCLE);
    assert_eq!(titles(&app), ["second.svg"]);
    assert_eq!(app.views.len(), 1, "the views follow the documents");
    assert!(app.ui.dialog.is_none(), "nothing asks to save the one it replaced");
}

#[test]
fn nothing_the_person_does_opens_or_closes_a_document() {
    let (mut app, inbox, host) = single();
    hand(&mut app, &host, "job.svg", SQUARE);
    for id in ["file.new", "file.newDialog", "file.newFromTemplate", "file.open", "file.openRecent1", "file.close", "file.closeAll", "app.home"] {
        let r = app.run(id, json!({}));
        assert!(r.as_ref().is_err_and(|e| e.contains("one file only")), "{id}: {r:?}");
        assert!(!menus::enabled(&app, id), "{id} is off");
        assert!(menus::shown_state(&app, id, &json!({})).is_none(), "{id} is left out of the menus");
    }
    // A tab's × and the window's Quit close nothing either.
    assert!(crate::unsaved::close(&mut app, 0).is_err_and(|e| e.contains("one file only")));
    assert!(crate::unsaved::close_all(&mut app, "quit").is_err_and(|e| e.contains("one file only")));
    assert!(!menus::enabled(&app, "app.quit"));
    // File › Open Recent Files has nothing to show, so it is left out.
    let file = &menus::menu_tree().into_iter().find(|(name, _)| *name == "File").unwrap().1;
    let recent = file.iter().find(|it| matches!(it, menus::Item::Sub("Open Recent Files", _))).unwrap();
    assert!(menus::entry(&app, recent).is_none(), "an empty submenu is left out");
    // A file the person opens or drops (the same inbox) is a document, so it isn't opened.
    inbox.lock().unwrap().push(("dropped.svg".into(), CIRCLE.to_vec()));
    app.drain_inbox();
    assert_eq!(titles(&app), ["job.svg"]);
    assert!(app.ui.status.contains("one file only"), "{}", app.ui.status);
    // There is no Home screen, even with nothing open yet.
    assert!(!menus::home_showing(&app));
    let (empty, ..) = single();
    assert!(!menus::home_showing(&empty));
}

#[test]
fn work_on_the_document_goes_on_as_usual() {
    let (mut app, _, host) = single();
    hand(&mut app, &host, "job.svg", SQUARE);
    for id in ["file.save", "file.saveAs", "edit.undo", "select.all"] {
        assert!(!menus::blocked_in_single_document(id), "{id}");
    }
    app.run("select.all", json!({})).unwrap();
    app.run("object.move", json!({ "dx": 5, "dy": 0 })).unwrap();
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(titles(&app), ["job.svg"]);
}

#[test]
fn without_the_flag_the_page_s_files_open_beside_the_others() {
    let (inbox, host): (Inbox, HostInbox) = (Arc::default(), Arc::default());
    let services = crate::Services { inbox: Some(inbox), host_inbox: Some(host.clone()), ..Default::default() };
    let mut app = VectorcraftApp::new(vectorcraft_engine::Session::new(), services);
    hand(&mut app, &host, "a.svg", SQUARE);
    hand(&mut app, &host, "b.svg", CIRCLE);
    assert_eq!(titles(&app), ["a.svg", "b.svg"]);
    assert!(app.run("file.new", json!({})).is_ok());
}

#[test]
fn host_is_told_when_open_completes_or_fails() {
    let (mut app, _, host) = single();
    let results = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = results.clone();
    app.services.host_open_result = Some(Box::new(move |result| captured.lock().unwrap().push(result)));
    hand(&mut app, &host, "good.svg", SQUARE);
    assert_eq!(*results.lock().unwrap(), [Ok(())]);
    hand(&mut app, &host, "bad.pdf", b"not a PDF");
    assert!(results.lock().unwrap().last().unwrap().is_err());
    assert_eq!(titles(&app), ["good.svg"], "failed import keeps the earlier document; host must invalidate its token");
}

#[test]
fn production_context_is_validated_before_replacing_the_file_and_installed_before_ack() {
    use vectorcraft_engine::cmd::production_separations::Context;
    let (mut app, _, host) = single();
    hand(&mut app, &host, "original.svg", SQUARE);
    let results = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = results.clone();
    app.services.host_open_result = Some(Box::new(move |r| captured.lock().unwrap().push(r)));
    host.lock().unwrap().push(HostOpen {
        name: "invalid.svg".into(),
        bytes: CIRCLE.to_vec(),
        print_production: false,
        separations: Some(Context::default()),
        raster_effects_ppi: None,
        units: None,
        stroke_units: None,
    });
    app.drain_inbox();
    assert_eq!(titles(&app), ["original.svg"]);
    assert!(results.lock().unwrap()[0].is_err());
    let context = Context { label: "Reporter file links".into(), ..Default::default() };
    host.lock().unwrap().push(HostOpen {
        name: "configured.svg".into(),
        bytes: CIRCLE.to_vec(),
        print_production: false,
        separations: Some(context.clone()),
        raster_effects_ppi: None,
        units: None,
        stroke_units: None,
    });
    app.drain_inbox();
    assert_eq!(titles(&app), ["configured.svg"]);
    assert_eq!(app.session.active().unwrap().production_separations.context, Some(context));
    assert_eq!(app.ui.open_panel.as_deref(), Some("productionSeparations"));
    assert_eq!(results.lock().unwrap().len(), 2, "one acknowledgement for each attempt");
    assert!(results.lock().unwrap()[1].is_ok());
    assert!(!app.session.active().unwrap().is_dirty());
}

#[test]
fn reopening_a_separation_keeps_its_docked_panel_in_place() {
    use vectorcraft_engine::cmd::production_separations::Context;
    let (mut app, _, host) = single();
    app.run("window.panel.dock", serde_json::json!({"panel":"productionSeparations", "onto":"properties"})).unwrap();
    let layout = serde_json::to_value(&app.ui.docking).unwrap();
    host.lock().unwrap().push(HostOpen {
        name: "docked.svg".into(),
        bytes: SQUARE.to_vec(),
        print_production: false,
        separations: Some(Context { label: "Reporter".into(), ..Default::default() }),
        raster_effects_ppi: None,
        units: None,
        stroke_units: None,
    });
    app.drain_inbox();
    assert_eq!(app.ui.open_panel, None, "a docked panel must not also pop out over the artwork");
    assert_eq!(serde_json::to_value(&app.ui.docking).unwrap(), layout);
    assert_eq!(titles(&app), ["docked.svg"]);
}

#[test]
fn the_page_sets_a_floor_for_the_raster_effects_resolution_without_marking_the_file_changed() {
    let (mut app, _, host) = single();
    let open = |host: &HostInbox, name: &str, bytes: Vec<u8>, ppi: Option<f64>| {
        host.lock().unwrap().push(HostOpen {
            name: name.into(),
            bytes,
            print_production: false,
            separations: None,
            raster_effects_ppi: ppi.map(|p: f64| p as u32),
            units: None,
            stroke_units: None,
        });
    };
    open(&host, "job.svg", SQUARE.to_vec(), Some(300.0));
    app.drain_inbox();
    let st = app.session.active().unwrap();
    assert_eq!(st.doc.raster_effects_ppi, 300.0, "a file at 72 ppi is raised to the page's 300");
    assert!(!st.is_dirty(), "nothing to save: the file is as it arrived plus the page's floor");
    // A file already finer keeps its own; without a floor nothing changes.
    app.run("document.rasterEffectsSettings", json!({"resolution": 600})).unwrap();
    let saved = vectorcraft_format::save_file(&app.session.active().unwrap().doc);
    open(&host, "fine.vectorcraft", saved, Some(300.0));
    app.drain_inbox();
    assert_eq!(app.session.active().unwrap().doc.raster_effects_ppi, 600.0);
    open(&host, "plain.svg", SQUARE.to_vec(), None);
    app.drain_inbox();
    assert_eq!(app.session.active().unwrap().doc.raster_effects_ppi, 72.0);
}

#[test]
fn the_raster_effects_floor_waits_for_a_pdf_behind_the_import_dialog() {
    // A two-page PDF opens after the Import PDF dialog: the floor applies to it when it arrives,
    // not to the document open before it (review editor-raster-300).
    let (mut app, _, host) = single();
    host.lock().unwrap().push(HostOpen {
        name: "first.svg".into(),
        bytes: SQUARE.to_vec(),
        print_production: false,
        separations: None,
        raster_effects_ppi: None,
        units: None,
        stroke_units: None,
    });
    app.drain_inbox();
    let page = || vectorcraft_testkit::pdf::PdfPage::new(100.0, 100.0, "0 0 1 rg 10 10 50 50 re f");
    let pdf = vectorcraft_testkit::pdf::pdf_with(&[page(), page()], &[], None);
    host.lock().unwrap().push(HostOpen {
        name: "two.pdf".into(),
        bytes: pdf,
        print_production: false,
        separations: None,
        raster_effects_ppi: Some(300),
        units: None,
        stroke_units: None,
    });
    app.drain_inbox();
    assert_eq!(titles(&app), ["first.svg"], "the Import PDF dialog is up");
    assert_eq!(app.session.active().unwrap().doc.raster_effects_ppi, 72.0, "the open document is not the page's file");
    // OK on the dialog (one headless frame with Enter pressed).
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let key = egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
    let mut out = ctx.run_ui(egui::RawInput { events: vec![key], ..Default::default() }, |ui| crate::dialogs::show(&mut app, ui.ctx()));
    out.textures_delta.clear();
    app.drain_inbox();
    let st = app.session.active().unwrap();
    assert_eq!((titles(&app), st.doc.raster_effects_ppi, st.is_dirty()), (vec!["two.pdf".to_string()], 300.0, false));
}

fn open_in(host: &HostInbox, name: &str, bytes: Vec<u8>, units: Option<Unit>, stroke_units: Option<Unit>) {
    host.lock().unwrap().push(HostOpen {
        name: name.into(),
        bytes,
        print_production: false,
        separations: None,
        raster_effects_ppi: None,
        units,
        stroke_units,
    });
}

#[test]
fn the_page_shows_its_job_in_its_units_without_an_edit_or_a_preference() {
    let (mut app, _, host) = single();
    app.session.prefs.units_general = "millimeters".into();
    app.session.prefs.units_stroke = "points".into();
    let prefs = serde_json::to_value(&app.session.prefs).unwrap();
    open_in(&host, "job.svg", SQUARE.to_vec(), Some(Unit::Inches), Some(Unit::Inches));
    app.drain_inbox();
    let st = app.session.active().unwrap();
    assert_eq!(st.doc.units, Unit::Inches, "the page's General unit, not the person's millimetres");
    assert!(!st.is_dirty(), "nothing to save: the units are a display choice");
    assert!(st.history.undo.is_empty() && st.history.redo.is_empty(), "and nothing to undo");
    assert_eq!((app.session.general_unit(), app.session.unit(Measure::Stroke)), (Unit::Inches, Unit::Inches));
    assert_eq!(serde_json::to_value(&app.session.prefs).unwrap(), prefs, "the stored preferences are the person's own");
    // The next job without units: the file's or the person's units again, strokes too.
    open_in(&host, "nz.svg", SQUARE.to_vec(), None, None);
    app.drain_inbox();
    assert_eq!((app.session.general_unit(), app.session.unit(Measure::Stroke)), (Unit::Millimeters, Unit::Points));
    assert_eq!(serde_json::to_value(&app.session.prefs).unwrap(), prefs);
}

#[test]
fn the_pages_units_override_a_native_document_saved_in_others() {
    let (mut app, _, host) = single();
    hand(&mut app, &host, "first.svg", SQUARE);
    app.run("document.setUnits", json!({"units": "millimeters"})).unwrap();
    let saved = vectorcraft_format::save_file(&app.session.active().unwrap().doc);
    open_in(&host, "saved.vectorcraft", saved.clone(), None, None);
    app.drain_inbox();
    assert_eq!(app.session.active().unwrap().doc.units, Unit::Millimeters, "a restored document keeps its saved units");
    open_in(&host, "saved.vectorcraft", saved, Some(Unit::Inches), None);
    app.drain_inbox();
    let st = app.session.active().unwrap();
    assert_eq!((st.doc.units, st.is_dirty(), st.history.undo.len()), (Unit::Inches, false, 0));
    // An edit afterwards is one undo step that keeps the units; undoing it is clean again.
    app.run("select.all", json!({})).unwrap();
    app.run("object.move", json!({ "dx": 5, "dy": 0 })).unwrap();
    app.run("edit.undo", json!({})).unwrap();
    let st = app.session.active().unwrap();
    assert_eq!((st.doc.units, st.is_dirty()), (Unit::Inches, false));
}

#[test]
fn the_pages_units_wait_for_a_pdf_behind_the_import_dialog() {
    let (mut app, _, host) = single();
    open_in(&host, "first.svg", SQUARE.to_vec(), Some(Unit::Millimeters), None);
    app.drain_inbox();
    let page = || vectorcraft_testkit::pdf::PdfPage::new(100.0, 100.0, "0 0 1 rg 10 10 50 50 re f");
    let pdf = vectorcraft_testkit::pdf::pdf_with(&[page(), page()], &[], None);
    open_in(&host, "two.pdf", pdf, Some(Unit::Inches), Some(Unit::Millimeters));
    app.drain_inbox();
    assert_eq!(titles(&app), ["first.svg"], "the Import PDF dialog is up");
    assert_eq!(app.session.active().unwrap().doc.units, Unit::Millimeters, "the open document is not the page's file");
    assert_eq!(app.session.unit(Measure::Stroke), Unit::Points, "nor are its strokes");
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let key = egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
    let mut out = ctx.run_ui(egui::RawInput { events: vec![key], ..Default::default() }, |ui| crate::dialogs::show(&mut app, ui.ctx()));
    out.textures_delta.clear();
    app.drain_inbox();
    let st = app.session.active().unwrap();
    assert_eq!((titles(&app), st.doc.units, st.is_dirty()), (vec!["two.pdf".to_string()], Unit::Inches, false));
    assert_eq!(app.session.unit(Measure::Stroke), Unit::Millimeters);
}

#[test]
fn choosing_a_stroke_unit_ends_the_pages() {
    let (mut app, _, host) = single();
    open_in(&host, "job.svg", SQUARE.to_vec(), None, Some(Unit::Inches));
    app.drain_inbox();
    assert_eq!(app.session.unit(Measure::Stroke), Unit::Inches);
    app.run("prefs.set", json!({"key": "unitsStroke", "value": "points"})).unwrap();
    assert_eq!(app.session.unit(Measure::Stroke), Unit::Inches, "the same preference again isn't a choice");
    app.run("prefs.set", json!({"key": "unitsStroke", "value": "millimeters"})).unwrap();
    assert_eq!(app.session.unit(Measure::Stroke), Unit::Millimeters);
}

#[test]
fn preferences_ok_keeps_the_persons_units_under_a_pages_job() {
    let (mut app, _, host) = single();
    app.session.prefs.units_general = "millimeters".into();
    app.session.prefs.units_stroke = "points".into();
    open_in(&host, "job.svg", SQUARE.to_vec(), Some(Unit::Inches), Some(Unit::Inches));
    app.drain_inbox();
    // Preferences, OK, Units untouched: the page's inches are not stored as the person's own.
    crate::prefs_dialog::open(&mut app, None);
    assert_eq!(app.ui.dialog.as_ref().unwrap().fields["unitsGeneral"], json!("inches"), "the dialog shows the document's units");
    crate::prefs_dialog::confirm(&mut app).unwrap();
    assert_eq!((app.session.prefs.units_general.as_str(), app.session.prefs.units_stroke.as_str()), ("millimeters", "points"));
    assert_eq!(app.ui.engine_prefs["unitsGeneral"], json!("millimeters"), "nor in the prefs the editor posts to the page");
    let st = app.session.active().unwrap();
    assert_eq!((st.doc.units, st.is_dirty(), app.session.unit(Measure::Stroke)), (Unit::Inches, false, Unit::Inches), "the job keeps its units");
    // A unit the person changes in the dialog is stored (and shown).
    crate::prefs_dialog::open(&mut app, None);
    let fields = &mut app.ui.dialog.as_mut().unwrap().fields;
    fields.insert("unitsGeneral".into(), json!("points"));
    fields.insert("unitsStroke".into(), json!("millimeters"));
    crate::prefs_dialog::confirm(&mut app).unwrap();
    assert_eq!((app.session.prefs.units_general.as_str(), app.session.prefs.units_stroke.as_str()), ("points", "millimeters"));
    assert_eq!((app.session.general_unit(), app.session.unit(Measure::Stroke)), (Unit::Points, Unit::Millimeters));
}
