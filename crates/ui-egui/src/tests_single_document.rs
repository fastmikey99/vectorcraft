//! A single-document editor (`Services::single_document`, the web build's `?host=parent`): only the
//! file the embedding page hands over is open, and nothing the person does opens another.

use std::sync::Arc;

use serde_json::json;

use crate::{HostInbox, HostOpen, Inbox, VectorcraftApp, menus};

const SQUARE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect x="10" y="10" width="20" height="20"/></svg>"#;
const CIRCLE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50"><circle cx="25" cy="25" r="10"/></svg>"#;

fn single() -> (VectorcraftApp, Inbox, HostInbox) {
    let (inbox, host): (Inbox, HostInbox) = (Arc::default(), Arc::default());
    let services = crate::Services { single_document: true, inbox: Some(inbox.clone()), host_inbox: Some(host.clone()), ..Default::default() };
    (VectorcraftApp::new(vectorcraft_engine::Session::new(), services), inbox, host)
}

fn hand(app: &mut VectorcraftApp, inbox: &HostInbox, name: &str, bytes: &[u8]) {
    inbox.lock().unwrap().push(HostOpen { name: name.to_string(), bytes: bytes.to_vec(), print_production: false, separations: None, checks: None });
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
        checks: None,
        separations: Some(Context::default()),
    });
    app.drain_inbox();
    assert_eq!(titles(&app), ["original.svg"]);
    assert!(results.lock().unwrap()[0].is_err());
    let context = Context { label: "Reporter file links".into(), ..Default::default() };
    host.lock().unwrap().push(HostOpen {
        name: "configured.svg".into(),
        bytes: CIRCLE.to_vec(),
        print_production: false,
        checks: None,
        separations: Some(context.clone()),
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
        checks: None,
        separations: Some(Context { label: "Reporter".into(), ..Default::default() }),
    });
    app.drain_inbox();
    assert_eq!(app.ui.open_panel, None, "a docked panel must not also pop out over the artwork");
    assert_eq!(serde_json::to_value(&app.ui.docking).unwrap(), layout);
    assert_eq!(titles(&app), ["docked.svg"]);
}
