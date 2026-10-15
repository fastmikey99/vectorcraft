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
//! - `vectorcraft:lineweight` (both ways): from the parent `{ id, minMm, gaps?, clear? }`, the open
//!   document's thin parts and narrow gaps against that minimum width, marked on a layer that never
//!   prints (`lineweight.check`); `clear` removes the marks. Answered with
//!   `vectorcraft:lineweight { id, minMm, thin, gaps, complete, skippedImages }` (counts; with
//!   `clear`, `{ id, cleared }`), or `vectorcraft:failed { error, id }`. The first change to the
//!   art after a check (an edit, an undo, another file; not a selection) is told once as
//!   `vectorcraft:lineweight { id, stale: true }`.
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
use std::sync::{Arc, Mutex};

use vectorcraft_ui_egui::{Services, VectorcraftApp};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

use crate::protocol;

type Inbox = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

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
    /// Line weight checks the parent asked for, run on the next frame.
    line_weights: Rc<RefCell<Vec<(JsValue, protocol::LineWeight)>>>,
    /// The last check answered and the document as it left it (by address): the first change to the
    /// art after it is told to the page once (`stale`), so it stops showing the counts as current.
    line_weight_watch: Rc<RefCell<Option<(JsValue, usize)>>>,
    /// Preferences the parent handed back, put on at the next frame (the last one wins).
    prefs: Rc<RefCell<Option<String>>>,
    /// The files the parent hands over (`Services::host_inbox`), which outlive graphics restarts.
    inbox: Inbox,
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
            line_weights: Rc::default(),
            line_weight_watch: Rc::default(),
            prefs: Rc::default(),
            inbox: Inbox::default(),
        })
    }

    /// Send what VectorCraft would download to the parent instead (each time the services are
    /// made, also after a graphics restart).
    pub fn connect(&self, services: &mut Services) {
        services.single_document = true;
        services.host_inbox = Some(self.inbox.clone());
        services.recovery_store = None;
        let bridge = self.clone();
        services.download = Some(Box::new(move |name: &str, bytes: &[u8]| {
            if let Err(e) = bridge.post_file(name, bytes, None, None, false) {
                log::error!("couldn't hand {name} to the page: {e}");
            }
        }));
    }

    pub fn post_ready(&self) {
        self.send(&[("type", "vectorcraft:ready".into()), ("version", env!("CARGO_PKG_VERSION").into()), ("singleDocument", true.into())]);
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
        let asked = std::mem::take(&mut *self.exports.borrow_mut());
        for (id, export) in asked {
            match write_export(app, &export) {
                Ok((name, bytes, bounds, native)) => {
                    if let Err(e) = self.post_file(&name, &bytes, Some(&id), bounds, native) {
                        log::error!("couldn't hand {name} to the page: {e}");
                    }
                }
                Err(error) => self.post_export_failed(&id, &error),
            }
        }
        let asked = std::mem::take(&mut *self.line_weights.borrow_mut());
        for (id, lw) in asked {
            match check_line_weight(app, &lw) {
                Ok(fields) => {
                    // Watch the document as the check left it (marks included); a clear watches nothing.
                    let watch = matches!(lw, protocol::LineWeight::Check { .. })
                        .then(|| app.session.active().map(|st| std::sync::Arc::as_ptr(&st.doc) as usize))
                        .flatten();
                    *self.line_weight_watch.borrow_mut() = watch.map(|doc| (id.clone(), doc));
                    let mut all = vec![("type", JsValue::from_str("vectorcraft:lineweight")), ("id", id)];
                    all.extend(fields);
                    self.send(&all);
                }
                Err(error) => self.post_export_failed(&id, &error),
            }
        }
        let asked = std::mem::take(&mut *self.places.borrow_mut());
        for (id, place) in asked {
            match place_file(app, &place) {
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
    pub fn report_dirty(&self, app: &VectorcraftApp) {
        // The art changed since the last check (an edit, an undo, another file): say so once.
        let current = app.session.active().map(|st| std::sync::Arc::as_ptr(&st.doc) as usize);
        let changed = self.line_weight_watch.borrow().as_ref().is_some_and(|(_, doc)| current != Some(*doc));
        if changed && let Some((id, _)) = self.line_weight_watch.borrow_mut().take() {
            self.send(&[("type", "vectorcraft:lineweight".into()), ("id", id), ("stale", true.into())]);
        }
        let dirty = app.session.documents().iter().any(|d| d.is_dirty());
        if self.reported.get() != Some(dirty) && self.send(&[("type", "vectorcraft:dirty".into()), ("dirty", dirty.into())]) {
            self.reported.set(Some(dirty));
        }
    }

    /// A `vectorcraft:save`; `id` when it answers a `vectorcraft:export`.
    /// `native`: the bytes are a selected image's own pixels, as embedded (not a render).
    fn post_file(&self, name: &str, bytes: &[u8], id: Option<&JsValue>, bounds: Option<vectorcraft_geom::Rect>, native: bool) -> Result<(), String> {
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let mut fields = vec![("type", "vectorcraft:save".into()), ("name", name.into()), ("bytes", buffer.clone().into())];
        if let Some(id) = id {
            fields.push(("id", id.clone()));
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
                match protocol::export(string(&data, "format"), flag("markSaved"), flag("selectedOnly"), flag("preserveEditing"), flag("imageOnly")) {
                    Ok(export) => {
                        bridge.exports.borrow_mut().push((id, export));
                        wake();
                    }
                    Err(error) => bridge.post_export_failed(&id, &error),
                }
                return;
            }
            if string(&data, "type").as_deref() == Some("vectorcraft:lineweight") {
                let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).unwrap_or(JsValue::UNDEFINED);
                let get = |k: &str| js_sys::Reflect::get(&data, &JsValue::from_str(k)).ok();
                let flag = |k: &str| get(k).and_then(|v| v.as_bool());
                match protocol::line_weight(get("minMm").and_then(|v| v.as_f64()), flag("gaps"), flag("clear")) {
                    Ok(lw) => {
                        bridge.line_weights.borrow_mut().push((id, lw));
                        wake();
                    }
                    Err(error) => bridge.post_export_failed(&id, &error),
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
                    bridge.inbox.lock().unwrap_or_else(|e| e.into_inner()).push((open.name, open.bytes));
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

/// The active document written as `export` asks → its file name and bytes.
/// → the file's name, its bytes, the selection's bounds (with `selected_only`), and whether the bytes
/// are a selected image's own pixels.
fn write_export(app: &mut VectorcraftApp, export: &protocol::Export) -> Result<(String, Vec<u8>, Option<vectorcraft_geom::Rect>, bool), String> {
    use vectorcraft_engine::cmd::fileio;
    let st = app.session.active().ok_or("vectorcraft:export: no document is open")?;
    let title = st.title();
    let stem = std::path::Path::new(&title).file_stem().map_or_else(|| title.clone(), |s| s.to_string_lossy().to_string());
    let selected = st.selection.objects.clone();
    // Only one image's own pixels will do: anything else is refused, saying what is selected.
    if export.image_only {
        let picked: Vec<protocol::Picked> = selected
            .iter()
            .map(|id| match st.doc.node(*id).map(|n| &n.kind) {
                Some(vectorcraft_doc::NodeKind::Image(_)) => protocol::Picked::Image,
                Some(vectorcraft_doc::NodeKind::Group { .. }) => protocol::Picked::Group,
                Some(vectorcraft_doc::NodeKind::Path { .. }) => protocol::Picked::Path,
                Some(vectorcraft_doc::NodeKind::Text(_)) => protocol::Picked::Type,
                _ => protocol::Picked::Other,
            })
            .collect();
        if let Some(why) = protocol::image_only_refusal(&picked) {
            return Err(why);
        }
    }
    if export.selected_only && selected.is_empty() {
        return Err("vectorcraft:export: select something to export (selectedOnly)".to_string());
    }
    // The selection's bounds in document points (what `vectorcraft:place` takes as `at`).
    let bounds =
        export.selected_only.then(|| selected.iter().filter_map(|id| st.doc.node(*id)?.geometric_bounds()).reduce(|a, b| a.union(b))).flatten();
    // One selected image, asked for as a raster: its own pixels as embedded, not drawn again.
    let one_image = match selected.as_slice() {
        [id] if export.selected_only && protocol::is_raster(export.format) => {
            st.doc.node(*id).filter(|n| matches!(n.kind, vectorcraft_doc::NodeKind::Image(_))).map(|_| *id)
        }
        _ => None,
    };
    if let Some(id) = one_image {
        let r = app.session.execute("links.unembed", &serde_json::json!({ "id": id.0 })).map_err(|e| e.to_string())?;
        let name = r.get("name").and_then(serde_json::Value::as_str).unwrap_or("image.png").to_string();
        let data = r.get("dataBase64").and_then(serde_json::Value::as_str).and_then(vectorcraft_format::base64_decode);
        return Ok((name, data.ok_or("vectorcraft:export: the image's pixels couldn't be read")?, bounds, true));
    }
    let mut params = serde_json::json!({ "selectedOnly": export.selected_only });
    if let (Some(keep), Some(o)) = (export.preserve_editing, params.as_object_mut()) {
        o.insert("preserveEditing".into(), keep.into());
    }
    let doc = fileio::export_source(st, &params).map_err(|e| e.to_string())?;
    let (bytes, _warnings) = fileio::encode_with_warnings(&doc, export.format, &params).map_err(|e| e.to_string())?;
    if export.mark_saved
        && let Some(st) = app.session.active_mut()
    {
        st.mark_saved();
    }
    Ok((format!("{stem}.{}", export.extension), bytes, bounds, false))
}

/// Place `place`'s file in place of the one selected object (with nothing selected, at `at`) →
/// the ids of what was placed. One undo step (`file.place`).
fn place_file(app: &mut VectorcraftApp, place: &protocol::Place) -> Result<Vec<u64>, String> {
    let st = app.session.active().ok_or("vectorcraft:place: no document is open")?;
    let selected = st.selection.objects.clone();
    let replace = match selected.as_slice() {
        [] => false,
        [_] => true,
        _ => return Err("vectorcraft:place: select the one object to replace".to_string()),
    };
    let mut params = serde_json::json!({
        "name": place.name,
        "dataBase64": vectorcraft_format::base64_encode(&place.bytes),
        "replace": replace,
        // The file's own frame (an SVG's viewBox, a PDF's page) is what fits, not its art's
        // bounds: a traced image whose art stops short of its edges keeps its size and place.
        "crop": "crop",
    });
    let Some(o) = params.as_object_mut() else { return Err("vectorcraft:place: couldn't build the command".to_string()) };
    match (place.at, replace) {
        (Some(at), _) => {
            o.insert("rect".into(), serde_json::json!(at));
        }
        // No rect: the selected object's own frame, through its transform (an image rotated,
        // mirrored or scaled keeps it, and art traced from its pixels lies where they did).
        (None, true) => {
            o.insert("fitReplaced".into(), true.into());
        }
        (None, false) => return Err("vectorcraft:place: select the object to replace, or give `at`".to_string()),
    }
    let r = app.run("file.place", params)?;
    Ok(r.get("ids").and_then(serde_json::Value::as_array).map(|ids| ids.iter().filter_map(serde_json::Value::as_u64).collect()).unwrap_or_default())
}

/// `{x, y, width, height}` under `key`, as four numbers (a value of another shape gives four that
/// fail the check, so it is refused rather than ignored).
fn rect(data: &JsValue, key: &str) -> Option<Vec<f64>> {
    let v = js_sys::Reflect::get(data, &JsValue::from_str(key)).ok().filter(|v| !v.is_undefined() && !v.is_null())?;
    let n = |k: &str| js_sys::Reflect::get(&v, &JsValue::from_str(k)).ok().and_then(|x| x.as_f64()).unwrap_or(f64::NAN);
    Some(vec![n("x"), n("y"), n("width"), n("height")])
}

/// How long a line weight check in the editor may take (s): it runs on the page's one thread.
const LINE_WEIGHT_SECONDS: u32 = 30;

/// Run `lw` on the open document (`lineweight.check` with its marks, or `lineweight.clear`) → the
/// answer's fields.
fn check_line_weight(app: &mut VectorcraftApp, lw: &protocol::LineWeight) -> Result<Vec<(&'static str, JsValue)>, String> {
    if app.session.active().is_none() {
        return Err("vectorcraft:lineweight: no document is open".to_string());
    }
    match lw {
        protocol::LineWeight::Clear => {
            let r = app.run("lineweight.clear", serde_json::json!({}))?;
            // `removed` counts the check layers taken away (none when there was no check).
            Ok(vec![("cleared", (r.get("removed").and_then(serde_json::Value::as_u64).unwrap_or(0) > 0).into())])
        }
        protocol::LineWeight::Check { min_mm, gaps } => {
            let r = app.run(
                "lineweight.check",
                serde_json::json!({ "min": format!("{min_mm} mm"), "gaps": gaps, "mark": true, "maxRegions": 1, "timeLimit": LINE_WEIGHT_SECONDS }),
            )?;
            let count = |k: &str| r.get(k).and_then(|v| v.get("count")).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
            Ok(vec![
                ("minMm", (*min_mm).into()),
                ("thin", count("thin").into()),
                ("gaps", count("gaps").into()),
                ("complete", r.get("complete").and_then(serde_json::Value::as_bool).unwrap_or(false).into()),
                ("skippedImages", r.get("skippedImages").and_then(serde_json::Value::as_f64).unwrap_or(0.0).into()),
            ])
        }
    }
}
