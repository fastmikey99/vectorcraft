//! The `postMessage` bridge to a same-origin parent page (`?host=parent`), for pages that embed
//! VectorCraft in an `<iframe>` (such as a Nextcloud app). The protocol is described for host
//! authors in `docs/web-embed.md`; its rules are in `protocol` (tested natively). Messages are plain
//! objects with a `type`:
//!
//! - `vectorcraft:ready` (to the parent): `{ version }`, the app is listening; send a file now.
//! - `vectorcraft:open` (from the parent): `{ name, bytes }`, bytes an `ArrayBuffer` or
//!   `Uint8Array`. It opens like a file picked with File › Open. Without a `name` or `bytes` it is
//!   refused with `vectorcraft:failed`.
//! - `vectorcraft:save` (to the parent): `{ name, bytes }` (a transferred `ArrayBuffer`) for every
//!   file VectorCraft would otherwise download (`Services::download`): Save, Save As, Save a
//!   Copy and exports. The parent stores it and reports failures itself.
//! - `vectorcraft:export` (from the parent): `{ id, format?, markSaved? }`, the active document
//!   written as `format` (PDF without it), answered with a `vectorcraft:save` carrying `id`, or a
//!   `vectorcraft:failed` carrying it when nothing is open or the format can't be written.
//!   With `selectedOnly`, only the selection: one selected image in a raster format comes as its
//!   own pixels as embedded, and the `save` carries the selection's `bounds` in document points.
//! - `vectorcraft:place` (from the parent): `{ id?, name, bytes, at? }`, the file placed in place of
//!   the one selected object (with nothing selected, at `at`): fitted inside `at` when given, else
//!   onto that object's own frame through its transform (an image's pixels, rotated or mirrored as
//!   they are), one undo step, selected. Answered with `vectorcraft:placed { id, ids }` or
//!   `vectorcraft:failed { error, id }`.
//! - `vectorcraft:prefs` (both ways): `{ prefs }`, the preferences and workspace as JSON text. The
//!   editor sends them when they change (the page stores them for the person signed in); the page
//!   sends them back before opening a file, and the editor takes them on. In host mode the editor
//!   keeps none of its own in the browser, so they follow the person, not the computer.
//! - `vectorcraft:dirty` (to the parent): `{ dirty }` whenever unsaved changes appear or go away.
//! - `vectorcraft:failed` (to the parent): `{ error }`. Before `ready`, the app couldn't start;
//!   after it, the last `vectorcraft:open` was refused (the app keeps running). With an `id`, the
//!   `vectorcraft:export` of that `id` was refused.
//!
//! The editor works on that one file (`Services::single_document`): nothing the person does
//! creates, opens or closes a document, there is no Home screen, and each `vectorcraft:open` takes
//! the open document's place without asking to save it (the page guards that with `dirty`). Data
//! Recovery is off, since restoring would add documents. `ready` says so with `singleDocument`.
//!
//! Only the parent window on the page's own origin is heard or answered, and a page with an opaque
//! origin (`null`) has no bridge. The parent guards leaving the page with unsaved changes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vectorcraft_ui_egui::{Services, VectorcraftApp};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

use crate::protocol;

type Inbox = vectorcraft_ui_egui::HostInbox;

#[derive(Clone)]
pub struct Bridge {
    window: web_sys::Window,
    parent: web_sys::Window,
    origin: String,
    /// The unsaved-changes state last reported to the parent.
    reported: Rc<Cell<Option<bool>>>,
    /// Exports the parent asked for, run on the next frame (they need the app).
    exports: Rc<RefCell<Vec<(JsValue, protocol::Export)>>>,
    /// Files the parent asked to place, run on the next frame.
    places: Rc<RefCell<Vec<(JsValue, protocol::Place)>>>,
    /// Preferences the parent handed back, put on at the next frame (the last one wins).
    prefs: Rc<RefCell<Option<String>>>,
    /// The files the parent hands over (`Services::host_inbox`), which outlive graphics restarts.
    inbox: Inbox,
    ready: Rc<Cell<bool>>,
    checks: Rc<RefCell<Option<(JsValue, serde_json::Value)>>>,
    check_results: Rc<RefCell<Option<(String, serde_json::Value)>>>,
    check_inflight: Rc<RefCell<Option<(String, f64)>>>,
    check_validity: Rc<RefCell<Option<(String, String)>>>,
    snapshot: Rc<RefCell<crate::check_snapshot::Snapshot>>,
    snapshot_validity: Rc<RefCell<Option<(String, String)>>>,
}

impl Bridge {
    /// `None` when the page isn't framed (there is no parent to talk to).
    pub fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok().filter(|o| !protocol::is_opaque(o))?;
        Some(Self {
            window,
            parent,
            origin,
            reported: Rc::default(),
            exports: Rc::default(),
            places: Rc::default(),
            prefs: Rc::default(),
            inbox: Inbox::default(),
            ready: Rc::default(),
            checks: Rc::default(),
            check_results: Rc::default(),
            check_inflight: Rc::default(),
            check_validity: Rc::default(),
            snapshot: Rc::default(),
            snapshot_validity: Rc::default(),
        })
    }

    /// Send what VectorCraft would download to the parent instead (each time the services are
    /// made, also after a graphics restart).
    pub fn connect(&self, services: &mut Services) {
        services.single_document = true;
        services.host_inbox = Some(self.inbox.clone());
        services.recovery_store = None;
        let bridge = self.clone();
        services.host_open_result = Some(Box::new(move |result| match result {
            Ok(()) => {
                bridge.send(&[("type", "vectorcraft:opened".into())]);
            }
            Err(error) => bridge.post_failed(&error),
        }));
        let bridge = self.clone();
        services.download = Some(Box::new(move |name: &str, bytes: &[u8]| bridge.post_file(name, bytes, None, None, false, false)));
    }

    pub fn post_ready(&self) {
        self.ready.set(true);
        self.send(&[
            ("type", "vectorcraft:ready".into()),
            ("version", env!("CARGO_PKG_VERSION").into()),
            ("singleDocument", true.into()),
            ("preserveSource", true.into()),
            ("openAcknowledgement", true.into()),
            ("printProduction", true.into()),
            ("productionSeparations", true.into()),
            ("separationChecks", true.into()),
            ("separationCheckService", true.into()),
        ]);
    }

    pub fn post_failed(&self, error: &str) {
        self.send(&[("type", "vectorcraft:failed".into()), ("error", error.into())]);
    }

    /// The refusal of the `vectorcraft:export` or `vectorcraft:place` with this `id`.
    fn post_export_failed(&self, id: &JsValue, error: &str) {
        log::warn!("refused an export for the page: {error}");
        self.send(&[("type", "vectorcraft:failed".into()), ("error", error.into()), ("id", id.clone())]);
    }

    /// Each frame: write the exports the parent asked for and send them back.
    pub fn run_exports(&self, app: &mut VectorcraftApp) {
        if let Some((id, request)) = self.checks.borrow_mut().take() {
            match app.run("production.checks.configure", serde_json::json!({"request":request})) {
                Ok(_) => {
                    self.send(&[("type", "vectorcraft:checks-configured".into()), ("id", id)]);
                }
                Err(error) => self.post_export_failed(&id, &error),
            }
        }
        if let Some((token, result)) = self.check_results.borrow_mut().take() {
            let params = serde_json::json!({"token":token,"report":result.get("report"),"error":result.get("error")});
            if let Err(error) = app.run("production.checks.complete", params) {
                log::warn!("refused service check result: {error}");
            }
            if self.check_inflight.borrow().as_ref().is_some_and(|(id, _)| id == &token) {
                *self.check_inflight.borrow_mut() = None;
            }
        }
        let active = app.session.active().and_then(|st| st.production_checks.remote_token()).map(String::from);
        let previous = self.check_inflight.borrow().clone();
        if let Some((token, started)) = previous {
            let expired = js_sys::Date::now() - started > 180_000.0;
            if active.as_deref() != Some(token.as_str()) || expired {
                self.send(&[("type", "vectorcraft:check-cancel".into()), ("id", token.clone().into())]);
                *self.check_inflight.borrow_mut() = None;
                if expired
                    && let Err(error) = app.run(
                        "production.checks.complete",
                        serde_json::json!({"token":token,"error":"Verification service exceeded the response deadline"}),
                    )
                {
                    log::warn!("could not finish expired check: {error}");
                }
            }
        }
        match vectorcraft_engine::cmd::production_checks::take_remote_export(&mut app.session) {
            Ok(Some((token, bytes))) => {
                self.snapshot.borrow_mut().replace(token.clone(), app.session.active().and_then(|st| st.production_checks.remote_document()));
                self.report_snapshot(app);
                let buffer = js_sys::Uint8Array::from(bytes.as_slice()).buffer();
                let payload = message(&[("type", "vectorcraft:check".into()), ("id", token.clone().into()), ("bytes", buffer.clone().into())]);
                let sent = payload.is_some_and(|m| self.parent.post_message_with_transfer(&m, &self.origin, &js_sys::Array::of1(&buffer)).is_ok());
                if sent {
                    *self.check_inflight.borrow_mut() = Some((token, js_sys::Date::now()));
                } else if let Err(error) = app
                    .run("production.checks.complete", serde_json::json!({"token":token,"error":"Could not submit PDF to the verification service"}))
                {
                    log::warn!("could not finish failed check submission: {error}");
                }
            }
            Ok(None) => {}
            Err(error) => log::warn!("could not export service check snapshot: {error}"),
        }
        let asked = std::mem::take(&mut *self.exports.borrow_mut());
        for (id, export) in asked {
            if let Some(token) = id.as_string() {
                self.snapshot.borrow_mut().replace(token, app.session.active().map(|st| st.doc.clone()));
                self.report_snapshot(app);
            }
            match crate::operations::write_export(app, &export) {
                Ok((name, bytes, bounds, native)) => match self.post_file(&name, &bytes, Some(&id), bounds, native, export.preserve_source) {
                    Ok(()) => {
                        if export.mark_saved
                            && let Some(st) = app.session.active_mut()
                        {
                            st.mark_saved();
                        }
                    }
                    Err(e) => self.post_export_failed(&id, &e),
                },
                Err(error) => self.post_export_failed(&id, &error),
            }
        }
        let asked = std::mem::take(&mut *self.places.borrow_mut());
        for (id, place) in asked {
            match crate::operations::place_file(app, &place) {
                Ok(ids) => {
                    let ids: js_sys::Array = ids.into_iter().map(|i| JsValue::from_f64(i as f64)).collect();
                    self.send(&[("type", "vectorcraft:placed".into()), ("id", id), ("ids", ids.into())]);
                }
                Err(error) => self.post_export_failed(&id, &error),
            }
        }
    }

    /// Preferences the parent handed back since the last frame, if any.
    pub fn take_prefs(&self) -> Option<String> {
        self.prefs.borrow_mut().take()
    }

    /// Hand the parent the preferences and workspace (they changed); false if it couldn't be sent.
    pub fn post_prefs(&self, text: &str) -> bool {
        self.send(&[("type", "vectorcraft:prefs".into()), ("prefs", text.into())])
    }

    /// After each frame: tell the parent when unsaved changes appear or go away.
    fn report_snapshot(&self, app: &VectorcraftApp) {
        let snapshot = self.snapshot.borrow();
        if snapshot.id.is_empty() {
            return;
        }
        let current = (snapshot.id.clone(), snapshot.validity(app.session.active().map(|st| &st.doc)).to_string());
        if self.snapshot_validity.borrow().as_ref() != Some(&current)
            && self.send(&[
                ("type", "vectorcraft:snapshot-validity".into()),
                ("id", current.0.as_str().into()),
                ("validity", current.1.as_str().into()),
            ])
        {
            *self.snapshot_validity.borrow_mut() = Some(current);
        }
    }

    pub fn report_dirty(&self, app: &VectorcraftApp) {
        self.report_snapshot(app);
        let dirty = app.session.documents().iter().any(|d| d.is_dirty());
        if self.reported.get() != Some(dirty) && self.send(&[("type", "vectorcraft:dirty".into()), ("dirty", dirty.into())]) {
            self.reported.set(Some(dirty));
        }
        let current = app.session.active().and_then(|st| {
            let report = st.production_checks.report.as_ref()?;
            let validity = match st.production_checks.validity(&st.doc) {
                vectorcraft_checks::Validity::Stale => "stale",
                _ => "provisional",
            };
            Some((report.run_id.clone(), validity.to_string()))
        });
        if *self.check_validity.borrow() != current {
            if let Some((run_id, validity)) = &current
                && !self.send(&[
                    ("type", "vectorcraft:check-validity".into()),
                    ("run_id", run_id.as_str().into()),
                    ("validity", validity.as_str().into()),
                ])
            {
                return;
            }
            *self.check_validity.borrow_mut() = current;
        }
    }

    /// A `vectorcraft:save`; `id` when it answers a `vectorcraft:export`.
    /// `native`: the bytes are a selected image's own pixels, as embedded (not a render).
    fn post_file(
        &self,
        name: &str,
        bytes: &[u8],
        id: Option<&JsValue>,
        bounds: Option<vectorcraft_geom::Rect>,
        native: bool,
        source_preserved: bool,
    ) -> Result<(), String> {
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let mut fields = vec![("type", "vectorcraft:save".into()), ("name", name.into()), ("bytes", buffer.clone().into())];
        if let Some(id) = id {
            fields.push(("id", id.clone()));
        }
        if source_preserved {
            fields.push(("sourcePreserved", true.into()));
        }
        if native {
            fields.push(("native", true.into()));
        }
        if let Some(b) = bounds
            && let Some(o) = message(&[("x", b.x0.into()), ("y", b.y0.into()), ("width", b.width().into()), ("height", b.height().into())])
        {
            fields.push(("bounds", o.into()));
        }
        let message = message(&fields).ok_or_else(|| "couldn't build the message".to_string())?;
        self.parent
            .post_message_with_transfer(&message, &self.origin, &js_sys::Array::of1(&buffer))
            .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))
    }

    /// Hear files from the parent: they go to its own inbox (`Services::host_inbox`), where each
    /// takes the open document's place; it outlives graphics restarts.
    /// `wake` asks the current runner for a frame. A `vectorcraft:open` that can't be opened is
    /// answered with `vectorcraft:failed`; a `vectorcraft:export` waits for the next frame
    /// ([`Bridge::run_exports`]); every other message is ignored (`protocol::accepts`).
    pub fn listen(&self, wake: impl Fn() + 'static) {
        let bridge = self.clone();
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let from_parent = event.source().is_some_and(|s| js_sys::Object::is(&s, &bridge.parent));
            let data = event.data();
            if !protocol::accepts(&bridge.origin, &event.origin(), from_parent, string(&data, "type").as_deref()) {
                return;
            }
            if string(&data, "type").as_deref() == Some("vectorcraft:export") {
                let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).unwrap_or(JsValue::UNDEFINED);
                let flag = |k: &str| js_sys::Reflect::get(&data, &JsValue::from_str(k)).ok().and_then(|v| v.as_bool());
                let request =
                    protocol::export(string(&data, "format"), flag("markSaved"), flag("selectedOnly"), flag("preserveEditing"), flag("imageOnly"))
                        .and_then(|export| {
                            let output = js_sys::Reflect::get(&data, &JsValue::from_str("output")).unwrap_or(JsValue::UNDEFINED);
                            let text = if output.is_undefined() {
                                None
                            } else {
                                Some(js_sys::JSON::stringify(&output).ok().and_then(|s| s.as_string()).unwrap_or_default())
                            };
                            protocol::with_output(export, text.as_deref())
                        });
                match request {
                    Ok(export) => {
                        bridge.exports.borrow_mut().push((id, export));
                        wake();
                    }
                    Err(error) => bridge.post_export_failed(&id, &error),
                }
                return;
            }
            if string(&data, "type").as_deref() == Some("vectorcraft:check-result") {
                let Some(token) = string(&data, "id").filter(|id| id.len() <= 256) else {
                    return;
                };
                if !bridge.check_inflight.borrow().as_ref().is_some_and(|(id, _)| id == &token) {
                    return;
                }
                let result = js_sys::JSON::stringify(&data)
                    .ok()
                    .and_then(|s| s.as_string())
                    .filter(|s| s.len() <= 32 * 1024 * 1024)
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .unwrap_or_else(|| serde_json::json!({"error":"Invalid or oversized service check result"}));
                *bridge.check_results.borrow_mut() = Some((token, result));
                wake();
                return;
            }
            if string(&data, "type").as_deref() == Some("vectorcraft:checks") {
                let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).unwrap_or(JsValue::UNDEFINED);
                let request = js_sys::Reflect::get(&data, &JsValue::from_str("request"))
                    .ok()
                    .and_then(|v| js_sys::JSON::stringify(&v).ok())
                    .and_then(|s| s.as_string())
                    .filter(|s| s.len() <= 16 * 1024 * 1024)
                    .ok_or_else(|| "Invalid or oversized check context".to_string())
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string()));
                match request {
                    Ok(request) => {
                        *bridge.checks.borrow_mut() = Some((id, request));
                        wake();
                    }
                    Err(error) => bridge.post_export_failed(&id, &error),
                }
                return;
            }
            if string(&data, "type").as_deref() == Some("vectorcraft:hello") {
                if bridge.ready.get() {
                    bridge.post_ready();
                }
                return;
            }
            if string(&data, "type").as_deref() == Some("vectorcraft:prefs") {
                match protocol::prefs(string(&data, "prefs")) {
                    Ok(text) => {
                        *bridge.prefs.borrow_mut() = Some(text);
                        wake();
                    }
                    Err(error) => bridge.post_failed(&error),
                }
                return;
            }
            if string(&data, "type").as_deref() == Some("vectorcraft:place") {
                let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).unwrap_or(JsValue::UNDEFINED);
                match protocol::place(string(&data, "name"), bytes(&data), rect(&data, "at")) {
                    Ok(place) => {
                        bridge.places.borrow_mut().push((id, place));
                        wake();
                    }
                    Err(error) => bridge.post_export_failed(&id, &error),
                }
                return;
            }
            match protocol::open(string(&data, "name"), bytes(&data)) {
                Ok(open) => {
                    let flag = js_sys::Reflect::get(&data, &JsValue::from_str("printProduction")).unwrap_or(JsValue::UNDEFINED);
                    if !flag.is_undefined() && flag.as_bool().is_none() {
                        bridge.post_failed("vectorcraft:open: printProduction must be a boolean");
                        return;
                    }
                    let value = js_sys::Reflect::get(&data, &JsValue::from_str("separations")).unwrap_or(JsValue::UNDEFINED);
                    let separations = if value.is_undefined() {
                        None
                    } else {
                        let result = js_sys::JSON::stringify(&value)
                            .ok()
                            .and_then(|s| s.as_string())
                            .filter(|s| s.len() <= protocol::MAX_PREFS)
                            .ok_or_else(|| "Invalid separation context".to_string())
                            .and_then(|s| {
                                serde_json::from_str::<vectorcraft_engine::cmd::production_separations::Context>(&s).map_err(|e| e.to_string())
                            })
                            .and_then(|c| c.validate().map(|()| c));
                        match result {
                            Ok(c) => Some(c),
                            Err(e) => {
                                bridge.post_failed(&e);
                                return;
                            }
                        }
                    };
                    let value = js_sys::Reflect::get(&data, &JsValue::from_str("checks")).unwrap_or(JsValue::UNDEFINED);
                    let checks = if value.is_undefined() || value.is_null() {
                        None
                    } else {
                        let parsed = js_sys::JSON::stringify(&value)
                            .ok()
                            .and_then(|s| s.as_string())
                            .filter(|s| s.len() <= 16 * 1024 * 1024)
                            .ok_or_else(|| "Check context exceeds the request budget".to_string())
                            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string()));
                        match parsed {
                            Ok(request) => Some(request),
                            Err(error) => {
                                bridge.post_failed(&error);
                                return;
                            }
                        }
                    };
                    bridge.inbox.lock().unwrap_or_else(|e| e.into_inner()).push(vectorcraft_ui_egui::HostOpen {
                        name: open.name,
                        bytes: open.bytes,
                        print_production: flag.as_bool().unwrap_or(false),
                        separations,
                        checks,
                    });
                    wake();
                }
                Err(error) => {
                    log::warn!("refused a file from the page: {error}");
                    bridge.post_failed(&error);
                }
            }
        });
        if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
            // The listener lives as long as the page.
            on_message.forget();
        }
    }

    /// Post a message to the parent; false if it couldn't be sent.
    fn send(&self, fields: &[(&str, JsValue)]) -> bool {
        message(fields).is_some_and(|m| self.parent.post_message(&m, &self.origin).is_ok())
    }
}

fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
    let m = js_sys::Object::new();
    for (key, value) in fields {
        js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
    }
    Some(m)
}

fn string(data: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
}

/// `bytes` as an `ArrayBuffer` or `Uint8Array`.
fn bytes(data: &JsValue) -> Option<Vec<u8>> {
    let bytes = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
    if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
        Some(array.to_vec())
    } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
        Some(js_sys::Uint8Array::new(&bytes).to_vec())
    } else {
        None
    }
}

/// `{x, y, width, height}` under `key`, as four numbers (a value of another shape gives four that
/// fail the check, so it is refused rather than ignored).
fn rect(data: &JsValue, key: &str) -> Option<Vec<f64>> {
    let v = js_sys::Reflect::get(data, &JsValue::from_str(key)).ok().filter(|v| !v.is_undefined() && !v.is_null())?;
    let n = |k: &str| js_sys::Reflect::get(&v, &JsValue::from_str(k)).ok().and_then(|x| x.as_f64()).unwrap_or(f64::NAN);
    Some(vec![n("x"), n("y"), n("width"), n("height")])
}
