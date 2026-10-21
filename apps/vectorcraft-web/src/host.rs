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
//! - `vectorcraft:open` may carry `rasterEffectsResolution` (ppi): the opened document's
//!   raster-effects resolution is raised to at least that, without marking it changed (a file read
//!   from a PDF starts at 72 ppi; shadows and glows are written at it). `ready` says
//!   `rasterEffectsResolution: true`.
//! - `vectorcraft:open` may carry `units` and `strokeUnits` (unit names such as `inches` or
//!   `millimeters`): the opened document's General unit and the Stroke unit while it is open,
//!   shown without marking it changed, adding an undo step or changing the person's preferences.
//!   `ready` says `units: true`.
//! - `vectorcraft:selection` (to the parent): `{ image, copies, otherSizes }` whenever it changes:
//!   whether exactly one image is selected, and how many other copies of it (same pixels) the
//!   document holds at the same printed size and at other sizes. `ready` says `selection: true`.
//! - `vectorcraft:place` (from the parent): `{ id?, name, bytes, at?, copies? }`, the file placed in place of
//!   the one selected object (with nothing selected, at `at`): fitted inside `at` when given, else
//!   onto that object's own frame through its transform (an image's pixels, rotated or mirrored as
//!   they are), one undo step, selected. Answered with `vectorcraft:placed { id, ids }` or
//!   `vectorcraft:failed { error, id }`.
//! - `vectorcraft:solidify` (from the parent): `{ id?, level?, copies? }`, Remove Soft Pixels on the
//!   one selected image (`object.image.solidifyAlpha`): every alpha at or above `level` (1–255,
//!   default 128) opaque, below it clear, colour samples kept; with `copies`, also its copies at
//!   the same printed size; one undo step. Answered with `vectorcraft:solidified { id, ids, form,
//!   level, softPixels, clearPixels, opaquePixels }` or `vectorcraft:failed { error, id }`. `ready`
//!   says `solidify: true`.
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
    /// The selection last reported to the parent, and the document revision it was read at.
    selection: Rc<Cell<Option<(u64, usize, protocol::Selection)>>>,
    /// Exports the parent asked for, run on the next frame (they need the app).
    exports: Rc<RefCell<Vec<(JsValue, protocol::Export)>>>,
    marks: Rc<RefCell<Vec<(JsValue, String, serde_json::Value)>>>,
    /// Files the parent asked to place, run on the next frame.
    places: Rc<RefCell<Vec<(JsValue, protocol::Place)>>>,
    /// Soft pixels the parent asked to remove, run on the next frame.
    solidifies: Rc<RefCell<Vec<(JsValue, protocol::Solidify)>>>,
    /// Preferences the parent handed back, put on at the next frame (the last one wins).
    prefs: Rc<RefCell<Option<String>>>,
    /// The files the parent hands over (`Services::host_inbox`), which outlive graphics restarts.
    inbox: Inbox,
    ready: Rc<Cell<bool>>,
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
            selection: Rc::default(),
            exports: Rc::default(),
            marks: Rc::default(),
            places: Rc::default(),
            solidifies: Rc::default(),
            prefs: Rc::default(),
            inbox: Inbox::default(),
            ready: Rc::default(),
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
            ("selection", true.into()),
            ("rasterEffectsResolution", true.into()),
            ("units", true.into()),
            ("solidify", true.into()),
            ("separationMarks", true.into()),
        ]);
    }

    pub fn post_failed(&self, error: &str) {
        self.send(&[("type", "vectorcraft:failed".into()), ("error", error.into())]);
    }

    /// The refusal of the `vectorcraft:export`, `vectorcraft:place` or `vectorcraft:solidify` with
    /// this `id`.
    fn post_export_failed(&self, id: &JsValue, error: &str) {
        log::warn!("refused an export for the page: {error}");
        self.send(&[("type", "vectorcraft:failed".into()), ("error", error.into()), ("id", id.clone())]);
    }

    /// Each frame: write the exports the parent asked for and send them back.
    pub fn run_exports(&self, app: &mut VectorcraftApp) {
        for (id, command, params) in std::mem::take(&mut *self.marks.borrow_mut()) {
            match app.run(&command, params).and_then(|_| app.session.execute("sep.mark.list", &serde_json::json!({})).map_err(|e| e.to_string())) {
                Ok(result) => match js_sys::JSON::parse(&result["marks"].to_string()) {
                    Ok(marks) => {
                        self.send(&[("type", "vectorcraft:sep.marks".into()), ("id", id), ("marks", marks), ("units", "pt".into())]);
                    }
                    Err(_) => self.post_export_failed(&id, "couldn't encode separation marks"),
                },
                Err(error) => self.post_export_failed(&id, &error),
            }
        }
        let asked = std::mem::take(&mut *self.exports.borrow_mut());
        for (id, export) in asked {
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
        let asked = std::mem::take(&mut *self.solidifies.borrow_mut());
        for (id, request) in asked {
            match crate::operations::solidify(app, &request) {
                Ok(done) => {
                    let ids: js_sys::Array = done.ids.into_iter().map(|i| JsValue::from_f64(i as f64)).collect();
                    self.send(&[
                        ("type", "vectorcraft:solidified".into()),
                        ("id", id),
                        ("ids", ids.into()),
                        ("form", done.form.as_str().into()),
                        ("level", (done.level as f64).into()),
                        ("softPixels", (done.soft as f64).into()),
                        ("clearPixels", (done.clear as f64).into()),
                        ("opaquePixels", (done.opaque as f64).into()),
                    ]);
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

    /// After each frame: tell the parent what is selected when it changes (`vectorcraft:selection`:
    /// `{ image, copies, otherSizes }`). Read again only when the document's revision moves.
    pub fn report_selection(&self, app: &VectorcraftApp) {
        let key = app.session.active().map_or((0, 0), |st| (st.revision, std::ptr::from_ref(st) as usize));
        let last = self.selection.get();
        if last.is_some_and(|(r, d, _)| (r, d) == key) {
            return;
        }
        let now = crate::operations::selection(app);
        if last.is_none_or(|(_, _, s)| s != now)
            && !self.send(&[
                ("type", "vectorcraft:selection".into()),
                ("image", now.image.into()),
                ("copies", (now.copies as f64).into()),
                ("otherSizes", (now.other_sizes as f64).into()),
            ])
        {
            return;
        }
        self.selection.set(Some((key.0, key.1, now)));
    }

    /// After each frame: tell the parent when unsaved changes appear or go away.
    pub fn report_dirty(&self, app: &VectorcraftApp) {
        let dirty = app.session.documents().iter().any(|d| d.is_dirty());
        if self.reported.get() != Some(dirty) && self.send(&[("type", "vectorcraft:dirty".into()), ("dirty", dirty.into())]) {
            self.reported.set(Some(dirty));
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
        let summary = crate::operations::saved_marks(name, bytes)?;
        let marks = js_sys::JSON::parse(&summary.to_string()).map_err(|_| "couldn't encode separation marks".to_string())?;
        fields.push(("marks", marks));
        fields.push(("markUnits", "pt".into()));
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
            if let Some(kind) = string(&data, "type").filter(|k| k.starts_with("vectorcraft:sep.mark.")) {
                let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).unwrap_or(JsValue::UNDEFINED);
                let mark = js_sys::Reflect::get(&data, &JsValue::from_str("mark"))
                    .ok()
                    .and_then(|m| js_sys::JSON::stringify(&m).ok())
                    .and_then(|m| m.as_string())
                    .and_then(|m| serde_json::from_str(&m).ok());
                match protocol::mark_command(&kind, mark) {
                    Ok((command, params)) => {
                        bridge.marks.borrow_mut().push((id, command.into(), params));
                        wake();
                    }
                    Err(error) => bridge.post_export_failed(&id, &error),
                }
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
                let copies = js_sys::Reflect::get(&data, &JsValue::from_str("copies")).ok().filter(|v| !v.is_undefined());
                if copies.as_ref().is_some_and(|v| v.as_bool().is_none()) {
                    bridge.post_export_failed(&id, "vectorcraft:place: copies must be a boolean");
                    return;
                }
                match protocol::place_copies(string(&data, "name"), bytes(&data), rect(&data, "at"), copies.and_then(|v| v.as_bool())) {
                    Ok(place) => {
                        bridge.places.borrow_mut().push((id, place));
                        wake();
                    }
                    Err(error) => bridge.post_export_failed(&id, &error),
                }
                return;
            }
            if string(&data, "type").as_deref() == Some("vectorcraft:solidify") {
                let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).unwrap_or(JsValue::UNDEFINED);
                // A field left out is `None`; one that is there but of the wrong type is `Some(None)`.
                let given = |key: &str| js_sys::Reflect::get(&data, &JsValue::from_str(key)).ok().filter(|v| !v.is_undefined());
                let level = given("level").map(|v| v.as_f64());
                let copies = given("copies").map(|v| v.as_bool());
                match protocol::solidify(level, copies) {
                    Ok(request) => {
                        bridge.solidifies.borrow_mut().push((id, request));
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
                    let ppi = js_sys::Reflect::get(&data, &JsValue::from_str("rasterEffectsResolution")).unwrap_or(JsValue::UNDEFINED);
                    let raster_effects_ppi = if ppi.is_undefined() {
                        None
                    } else {
                        match ppi.as_f64().filter(|p| p.is_finite() && (1.0..=2400.0).contains(p)) {
                            Some(p) => Some(p.round() as u32),
                            None => {
                                bridge.post_failed("vectorcraft:open: rasterEffectsResolution must be a ppi from 1 to 2400");
                                return;
                            }
                        }
                    };
                    // A field left out is `None`; one that is there but not a string is `Some(None)`.
                    let given = |key: &str| {
                        let v = js_sys::Reflect::get(&data, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED);
                        (!v.is_undefined()).then(|| v.as_string())
                    };
                    let units = match protocol::open_unit("units", given("units")) {
                        Ok(u) => u,
                        Err(e) => {
                            bridge.post_failed(&e);
                            return;
                        }
                    };
                    let stroke_units = match protocol::open_unit("strokeUnits", given("strokeUnits")) {
                        Ok(u) => u,
                        Err(e) => {
                            bridge.post_failed(&e);
                            return;
                        }
                    };
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
                    bridge.inbox.lock().unwrap_or_else(|e| e.into_inner()).push(vectorcraft_ui_egui::HostOpen {
                        name: open.name,
                        bytes: open.bytes,
                        print_production: flag.as_bool().unwrap_or(false),
                        separations,
                        raster_effects_ppi,
                        units,
                        stroke_units,
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
